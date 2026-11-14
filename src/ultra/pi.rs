//! PI (parallel interface) DMA. As on the console, a transfer's bytes and its reply land together
//! when it completes, never inside the call that starts it. Transfers wait in one queue served in
//! order, one per completion, as the serial PI raises one interrupt per transfer. Nothing models
//! the cart's transfer time. A completion stands in for the PI interrupt at three places, each
//! with no other guest running: a guest's own send or receive (`complete_next` from
//! `mesg::scheduling_point`), the scheduler finding no guest ready (`complete_next`), and a guest
//! polling a reply queue a pending transfer will post to (`complete_through`). The stand-in is
//! early when a message call follows the request sooner than the cart would deliver, and late
//! when a guest runs a long stretch without one.

use std::collections::VecDeque;
use std::os::raw::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Mutex;

use crate::ultra::mesg;

const OS_READ: i32 = 0; // device -> RDRAM (include/PR/os_pi.h)
const OS_WRITE: i32 = 1; // RDRAM -> device

struct Transfer {
    src: usize,
    dst: usize,
    len: usize,
    queue: usize,
    mb: usize,
}

static PENDING: Mutex<VecDeque<Transfer>> = Mutex::new(VecDeque::new());

/// Queue a copy of `len` bytes and its reply. Both buffers must outlive the transfer; the caller
/// checked that `queue` exists.
pub(crate) fn start(src: *const u8, dst: *mut u8, len: usize, queue: usize, mb: usize) {
    PENDING.lock().unwrap().push_back(Transfer {
        src: src as usize,
        dst: dst as usize,
        len,
        queue,
        mb,
    });
}

fn complete(t: Transfer) {
    // A full reply queue drops the reply; the bytes still land.
    mesg::post_with(t.queue, t.mb, || {
        if t.len > 0 {
            // Guest source and destination buffers may overlap.
            unsafe { std::ptr::copy(t.src as *const u8, t.dst as *mut u8, t.len) };
        }
    });
}

/// Complete the oldest pending transfer. Returns whether there was one.
pub(crate) fn complete_next() -> bool {
    let next = PENDING.lock().unwrap().pop_front();
    next.map(complete).is_some()
}

/// Complete every transfer up to and including the first that replies to `queue`. Returns
/// whether one did.
pub(crate) fn complete_through(queue: usize) -> bool {
    let done: Vec<Transfer> = {
        let mut pending = PENDING.lock().unwrap();
        match pending.iter().position(|t| t.queue == queue) {
            Some(i) => pending.drain(..=i).collect(),
            None => return false,
        }
    };
    done.into_iter().for_each(complete);
    true
}

#[no_mangle]
pub extern "C" fn R64PiStartDma(
    mb: *mut c_void,
    dir: i32,
    dev_addr: usize,
    v_addr: *mut c_void,
    nbytes: usize,
    mq: *mut c_void,
) -> i32 {
    // Queue or scheduler lock poisoning must not unwind across the C boundary.
    catch_unwind(AssertUnwindSafe(|| {
        if mb.is_null() || mq.is_null() || nbytes > isize::MAX as usize {
            return -1;
        }
        if dir != OS_READ && dir != OS_WRITE {
            return -1;
        }
        if nbytes > 0
            && (v_addr.is_null()
                || dev_addr == 0
                || dev_addr.checked_add(nbytes).is_none()
                || (v_addr as usize).checked_add(nbytes).is_none())
        {
            return -1;
        }
        if !mesg::registered(mq as usize) {
            return -1;
        }
        let dev = dev_addr as *mut u8;
        let v = v_addr as *mut u8;
        if dir == OS_READ {
            start(dev, v, nbytes, mq as usize, mb as usize);
        } else {
            start(v, dev, nbytes, mq as usize, mb as usize);
        }
        0
    }))
    .unwrap_or(-1)
}

/// Serializes tests that start or force-complete transfers, which share the one queue.
#[cfg(test)]
pub(crate) fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
pub(crate) fn complete_all() {
    while complete_next() {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ultra::mesg;
    use crate::ultra::mesg::{R64MesgQueueCreate, R64MesgRecv};
    use std::ffi::c_void;
    use std::ptr;
    use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};

    // Leak unique queue/mesg storage so parallel tests can't alias each other's registrations via
    // reused stack addresses.
    fn fresh_queue(cap: usize) -> *mut c_void {
        let backing = Box::leak(Box::new([0u8; 64])).as_mut_ptr() as *mut c_void;
        let slots: &'static mut [*mut c_void] =
            Box::leak(vec![ptr::null_mut::<c_void>(); cap].into_boxed_slice());
        R64MesgQueueCreate(backing, slots.as_mut_ptr(), cap as i32);
        backing
    }
    fn fresh_mb() -> *mut c_void {
        Box::leak(Box::new([0u8; 16])).as_mut_ptr() as *mut c_void
    }

    #[test]
    fn read_copies_and_posts_completion() {
        let _pi = test_lock();
        let src = [1u8, 2, 3, 4];
        let mut dst = [0u8; 4];
        let mq = fresh_queue(1);
        let mb = fresh_mb();
        let r = R64PiStartDma(
            mb,
            0,
            src.as_ptr() as usize,
            dst.as_mut_ptr() as *mut c_void,
            4,
            mq,
        );
        assert_eq!(r, 0);
        complete_all();
        assert_eq!(dst, src);
        let (ret, m) = mesg::recv(mq as usize, 0);
        assert_eq!(ret, 0);
        assert_eq!(m, mb as usize);
    }

    #[test]
    fn dma_copies_exactly_nbytes() {
        let _pi = test_lock();
        let mq = fresh_queue(4);
        let src: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];
        let mut dst: [u8; 8] = [0xFF; 8];
        let mb = fresh_mb();
        let rc = R64PiStartDma(
            mb,
            0,
            src.as_ptr() as usize,
            dst.as_mut_ptr() as *mut c_void,
            4,
            mq,
        );
        assert_eq!(rc, 0);
        complete_all();
        assert_eq!(&dst[..4], &src[..4]);
        assert_eq!(&dst[4..], &[0xFF; 4], "copied exactly nbytes");
        let mut out: *mut c_void = ptr::null_mut();
        assert_eq!(R64MesgRecv(mq, &mut out, 0), 0);
        assert_eq!(out, mb);
    }

    #[test]
    fn full_retqueue_still_copies_and_drops_the_completion() {
        let _pi = test_lock();
        let mq = fresh_queue(1);
        assert_eq!(mesg::send(mq as usize, 0x1, 0), 0);
        let src = [1u8, 2, 3, 4];
        let mut dst = [9u8; 4];
        let r = R64PiStartDma(
            fresh_mb(),
            0,
            src.as_ptr() as usize,
            dst.as_mut_ptr() as *mut c_void,
            4,
            mq,
        );
        assert_eq!(r, 0);
        complete_all();
        assert_eq!(dst, src);
        let mut out: *mut c_void = ptr::null_mut();
        assert_eq!(R64MesgRecv(mq, &mut out, 0), 0);
        assert_eq!(out as usize, 0x1);
        assert_eq!(
            R64MesgRecv(mq, &mut out, 0),
            -1,
            "the completion was dropped"
        );
    }

    #[test]
    fn missing_retqueue_errors_no_panic() {
        let fake = Box::leak(Box::new([0u8; 64])).as_mut_ptr() as *mut c_void;
        let src = [1u8; 4];
        let mut dst = [0u8; 4];
        assert_eq!(
            R64PiStartDma(
                fresh_mb(),
                0,
                src.as_ptr() as usize,
                dst.as_mut_ptr() as *mut c_void,
                4,
                fake
            ),
            -1
        );
    }

    #[test]
    fn null_and_invalid_pointers_error() {
        let mq = fresh_queue(1);
        let mb = fresh_mb();
        let dummy = ptr::dangling_mut::<c_void>(); // non-null; guards return before any deref
        assert_eq!(
            R64PiStartDma(ptr::null_mut(), 0, 1, dummy, 4, mq),
            -1,
            "null mb"
        );
        assert_eq!(
            R64PiStartDma(mb, 0, 1, dummy, 4, ptr::null_mut()),
            -1,
            "null mq"
        );
        assert_eq!(
            R64PiStartDma(mb, 0, 1, ptr::null_mut(), 4, mq),
            -1,
            "null v_addr, nbytes>0"
        );
        let mut d = [0u8; 4];
        assert_eq!(
            R64PiStartDma(mb, 0, 0, d.as_mut_ptr() as *mut c_void, 4, mq),
            -1,
            "zero dev_addr, nbytes>0"
        );
    }

    #[test]
    fn oversize_and_overflow_error() {
        let mq = fresh_queue(1);
        let mb = fresh_mb();
        let mut d = [0u8; 4];
        let dp = d.as_mut_ptr() as *mut c_void;
        assert_eq!(
            R64PiStartDma(mb, 0, 1, dp, isize::MAX as usize + 1, mq),
            -1,
            "nbytes > isize::MAX"
        );
        assert_eq!(
            R64PiStartDma(mb, 0, usize::MAX, dp, 4, mq),
            -1,
            "dev_addr + nbytes wraps"
        );
    }

    #[test]
    fn overlapping_copy_uses_memmove() {
        let _pi = test_lock();
        let mq = fresh_queue(1);
        let mut buf = [1u8, 2, 3, 4, 0];
        let base = buf.as_mut_ptr();
        let src = base as usize;
        let dst = unsafe { base.add(1) } as *mut c_void;
        let r = R64PiStartDma(fresh_mb(), 0, src, dst, 4, mq);
        assert_eq!(r, 0);
        complete_all();
        assert_eq!(buf, [1u8, 1, 2, 3, 4], "memmove shifted correctly");
    }

    #[test]
    fn zero_size_is_noop_but_completes() {
        let _pi = test_lock();
        let mq = fresh_queue(1);
        let mb = fresh_mb();
        assert_eq!(R64PiStartDma(mb, 0, 0, ptr::null_mut(), 0, mq), 0);
        complete_all();
        let (ret, m) = mesg::recv(mq as usize, 0);
        assert_eq!(ret, 0);
        assert_eq!(m, mb as usize);
    }

    #[test]
    fn write_direction_copies_ram_to_device() {
        let _pi = test_lock();
        let mut dev = [0u8; 4];
        let src = [9u8, 8, 7, 6];
        let mq = fresh_queue(1);
        let mb = fresh_mb();
        let r = R64PiStartDma(
            mb,
            1,
            dev.as_mut_ptr() as usize,
            src.as_ptr() as *mut c_void,
            4,
            mq,
        );
        assert_eq!(r, 0);
        complete_all();
        assert_eq!(dev, src, "write copied RDRAM into the device buffer");
        assert_eq!(mesg::recv(mq as usize, 0), (0, mb as usize));
    }

    #[test]
    fn invalid_direction_rejected_no_side_effect() {
        let _pi = test_lock();
        let src = [1u8, 2, 3, 4];
        let mut dst = [0u8; 4];
        let mq = fresh_queue(1);
        assert_eq!(
            R64PiStartDma(
                fresh_mb(),
                2,
                src.as_ptr() as usize,
                dst.as_mut_ptr() as *mut c_void,
                4,
                mq
            ),
            -1
        );
        complete_all();
        assert_eq!(dst, [0u8; 4], "no copy on an invalid direction");
        assert_eq!(
            mesg::recv(mq as usize, 0).0,
            -1,
            "no completion on an invalid direction"
        );
    }

    static SRC: [u8; 4] = [1, 2, 3, 4];
    static REPLY_Q: AtomicUsize = AtomicUsize::new(0);
    static OTHER_Q: AtomicUsize = AtomicUsize::new(0);
    static DST: AtomicUsize = AtomicUsize::new(0);
    static REQUESTER: AtomicUsize = AtomicUsize::new(0);
    static AT_START: AtomicIsize = AtomicIsize::new(-1);
    static BY_LOWER: AtomicIsize = AtomicIsize::new(-1);
    static AFTER_REPLY: AtomicIsize = AtomicIsize::new(-1);
    static UNRELATED_POLL: AtomicIsize = AtomicIsize::new(0);
    static SEQ: AtomicIsize = AtomicIsize::new(0);
    static REPLY_SEQ: AtomicIsize = AtomicIsize::new(-1);
    static SENT_SEQ: AtomicIsize = AtomicIsize::new(-1);

    fn dst_byte() -> isize {
        unsafe { *(DST.load(Ordering::SeqCst) as *const u8) as isize }
    }
    fn start_into_dst() -> i32 {
        R64PiStartDma(
            fresh_mb(),
            OS_READ,
            SRC.as_ptr() as usize,
            DST.load(Ordering::SeqCst) as *mut c_void,
            4,
            REPLY_Q.load(Ordering::SeqCst) as *mut c_void,
        )
    }
    fn setup() {
        REPLY_Q.store(fresh_queue(1) as usize, Ordering::SeqCst);
        OTHER_Q.store(fresh_queue(1) as usize, Ordering::SeqCst);
        DST.store(
            Box::leak(Box::new([0u8; 4])).as_mut_ptr() as usize,
            Ordering::SeqCst,
        );
        SEQ.store(0, Ordering::SeqCst);
        for seen in [&AT_START, &BY_LOWER, &AFTER_REPLY, &REPLY_SEQ, &SENT_SEQ] {
            seen.store(-1, Ordering::SeqCst);
        }
    }
    fn spawn(entry: extern "C" fn(*mut c_void), pri: i32) -> *mut c_void {
        let t = Box::leak(Box::new([0u8; 128])).as_mut_ptr() as *mut c_void;
        crate::ultra::thread::R64ThreadCreate(t, 1, entry, ptr::null_mut(), ptr::null_mut(), pri);
        t
    }
    fn wait_for(seen: &AtomicIsize) {
        for _ in 0..2000 {
            if seen.load(Ordering::SeqCst) != -1 {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("guest never got there");
    }

    extern "C" fn requester(_: *mut c_void) {
        assert_eq!(start_into_dst(), 0);
        AT_START.store(dst_byte(), Ordering::SeqCst);
        let _ = mesg::recv(REPLY_Q.load(Ordering::SeqCst), 1);
        AFTER_REPLY.store(dst_byte(), Ordering::SeqCst);
        REPLY_SEQ.store(SEQ.fetch_add(1, Ordering::SeqCst), Ordering::SeqCst);
    }

    extern "C" fn lower(_: *mut c_void) {
        // The higher-priority requester runs at once and blocks on its reply, so this runs first.
        crate::ultra::thread::R64ThreadStart(REQUESTER.load(Ordering::SeqCst) as *mut c_void);
        BY_LOWER.store(dst_byte(), Ordering::SeqCst);
        let _ = mesg::recv(OTHER_Q.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn transfer_completes_when_no_guest_is_ready() {
        let _pi = test_lock();
        setup();
        REQUESTER.store(spawn(requester, 10) as usize, Ordering::SeqCst);
        crate::ultra::thread::R64ThreadStart(spawn(lower, 5));
        wait_for(&AFTER_REPLY);
        wait_for(&BY_LOWER);
        assert_eq!(
            AT_START.load(Ordering::SeqCst),
            0,
            "no bytes inside the call"
        );
        assert_eq!(
            BY_LOWER.load(Ordering::SeqCst),
            0,
            "a ready guest runs before completion"
        );
        assert_eq!(
            AFTER_REPLY.load(Ordering::SeqCst),
            1,
            "bytes land with the reply"
        );
        mesg::send(OTHER_Q.load(Ordering::SeqCst), 0, 0);
    }

    extern "C" fn poller(_: *mut c_void) {
        assert_eq!(start_into_dst(), 0);
        let (ret, _) = mesg::recv(OTHER_Q.load(Ordering::SeqCst), 0);
        UNRELATED_POLL.store(ret as isize, Ordering::SeqCst);
        AT_START.store(dst_byte(), Ordering::SeqCst);
        let (ret, _) = mesg::recv(REPLY_Q.load(Ordering::SeqCst), 0);
        assert_eq!(ret, 0);
        AFTER_REPLY.store(dst_byte(), Ordering::SeqCst);
    }

    #[test]
    fn polling_a_reply_queue_completes_its_transfer() {
        let _pi = test_lock();
        setup();
        crate::ultra::thread::R64ThreadStart(spawn(poller, 10));
        wait_for(&AFTER_REPLY);
        assert_eq!(UNRELATED_POLL.load(Ordering::SeqCst), -1);
        assert_eq!(
            AT_START.load(Ordering::SeqCst),
            0,
            "an unrelated poll completes nothing"
        );
        assert_eq!(AFTER_REPLY.load(Ordering::SeqCst), 1);
    }

    extern "C" fn lower_sender(_: *mut c_void) {
        crate::ultra::thread::R64ThreadStart(REQUESTER.load(Ordering::SeqCst) as *mut c_void);
        assert_eq!(mesg::send(OTHER_Q.load(Ordering::SeqCst), 0, 0), 0);
        SENT_SEQ.store(SEQ.fetch_add(1, Ordering::SeqCst), Ordering::SeqCst);
        BY_LOWER.store(dst_byte(), Ordering::SeqCst);
    }

    #[test]
    fn a_lower_guests_send_completes_the_transfer_for_a_higher_waiter() {
        let _pi = test_lock();
        setup();
        REQUESTER.store(spawn(requester, 10) as usize, Ordering::SeqCst);
        crate::ultra::thread::R64ThreadStart(spawn(lower_sender, 5));
        wait_for(&SENT_SEQ);
        wait_for(&REPLY_SEQ);
        assert!(
            REPLY_SEQ.load(Ordering::SeqCst) < SENT_SEQ.load(Ordering::SeqCst),
            "the waiter resumes before the sender continues"
        );
        assert_eq!(AFTER_REPLY.load(Ordering::SeqCst), 1);
        assert_eq!(BY_LOWER.load(Ordering::SeqCst), 1);
    }
}
