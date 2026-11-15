//! RCP task engine: routes each OSTask through its guest-registered microcode handler.
//! A graphics task holds the guest run token while the render thread walks its display list,
//! then posts SP. The render thread finishes the task without the token; its patches are
//! applied and DP is posted under the token later (see `Completion`). Audio and njpgdsp
//! handlers run in place and post SP.

use std::os::raw::c_void;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Condvar, Mutex, OnceLock};

use crate::ultra::event::{self, OS_EVENT_DP, OS_EVENT_SP};
use crate::ultra::rsp::{self, Handler};

use super::acmd::{native::NativeContext, Microcode};

const M_GFXTASK: u32 = 1;

/// Host ABI of OSTask_t in include/libultra/os_sp.h.
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub(crate) struct OSTaskT {
    pub type_: u32,
    pub flags: u32,
    pub ucode_boot: *mut u64,
    pub ucode_boot_size: u32,
    pub ucode: *mut u64,
    pub ucode_size: u32,
    pub ucode_data: *mut u64,
    pub ucode_data_size: u32,
    pub dram_stack: *mut u64,
    pub dram_stack_size: u32,
    pub output_buff: *mut u64,
    pub output_buff_size: *mut u64,
    pub data_ptr: *mut u64,
    pub data_size: u32,
    pub yield_data_ptr: *mut u64,
    pub yield_data_size: u32,
}

///
/// # Safety
/// `task` must point at a live OSTask whose layout matches `OSTaskT`.
unsafe fn read_task(task: *const c_void) -> (u32, usize, usize) {
    let t = &*(task as *const OSTaskT);
    (t.type_, t.data_ptr as usize, t.ucode as usize)
}

fn skip_completion_events(task_type: u32) -> &'static [i32] {
    if task_type == M_GFXTASK {
        &[OS_EVENT_SP, OS_EVENT_DP]
    } else {
        &[OS_EVENT_SP]
    }
}

/// Where the last graphics task is between its walk and its DP.
enum Completion {
    Idle,
    /// Submitted: the render thread is walking or finishing it.
    InFlight,
    /// Finished: its patches wait to be applied under the token, then DP.
    Ready(Vec<ultragfx::GuestWrite>),
}

struct Pending {
    state: Mutex<Completion>,
    ready: Condvar,
}

impl Pending {
    const fn new() -> Self {
        Pending {
            state: Mutex::new(Completion::Idle),
            ready: Condvar::new(),
        }
    }

    /// A task is submitted. A previous one still finishing or ready keeps its state: it completes
    /// before this task's walk.
    fn submit(&self) {
        let mut state = self.state.lock().unwrap();
        if matches!(*state, Completion::Idle) {
            *state = Completion::InFlight;
        }
    }

    fn finish(&self, writes: Vec<ultragfx::GuestWrite>) {
        *self.state.lock().unwrap() = Completion::Ready(writes);
        self.ready.notify_all();
    }

    fn abandon(&self) {
        *self.state.lock().unwrap() = Completion::Idle;
    }

    fn is_ready(&self) -> bool {
        matches!(*self.state.lock().unwrap(), Completion::Ready(_))
    }

    fn take_ready(&self) -> Option<Vec<ultragfx::GuestWrite>> {
        let mut state = self.state.lock().unwrap();
        match std::mem::replace(&mut *state, Completion::Idle) {
            Completion::Ready(writes) => Some(writes),
            other => {
                *state = other;
                None
            }
        }
    }

    fn wait_while_in_flight(&self) {
        let mut state = self.state.lock().unwrap();
        while matches!(*state, Completion::InFlight) {
            #[cfg(test)]
            tests::WAITING.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            state = self.ready.wait(state).unwrap();
        }
    }
}

fn pending() -> &'static Pending {
    static PENDING: Pending = Pending::new();
    &PENDING
}

/// Apply a finished task's patches and post DP. The caller holds the run token, or is the host
/// thread `sched::run_as_host` let in. DP follows the SP the guest posted when the walk ended:
/// a task is only `Ready` after its walk, and the holder posts SP before its next scheduling point.
pub(crate) fn complete_ready() -> bool {
    if !pending().is_ready() {
        return false;
    }
    assert!(
        super::sched::guests_excluded(),
        "graphics write-back requires exclusive guest memory access"
    );
    let Some(writes) = pending().take_ready() else {
        return false;
    };
    // SAFETY: no guest runs while the token is held, and every patch lies in guest memory that
    // ultragfx bounds-checked during the walk.
    unsafe { crate::render::apply_guest_writes(writes) };
    event::post(OS_EVENT_DP);
    true
}

pub(crate) fn complete_live() -> bool {
    !crate::gamepad::script::replaying() && complete_ready()
}

/// Render thread: the task is finished. Outside replay, complete it now if the token allows.
pub(crate) fn finished(writes: Vec<ultragfx::GuestWrite>) {
    pending().finish(writes);
    if !crate::gamepad::script::replaying() {
        super::sched::run_as_host(|| {
            complete_ready();
        });
    }
}

/// Render thread, before walking a task: the previous one's patches must be in guest memory.
/// The submitting guest is parked in `submit_and_wait`, so `run_as_host` lets this thread in.
pub(crate) fn complete_before_walk() {
    if pending().is_ready() {
        let ran = super::sched::run_as_host(|| {
            complete_ready();
        });
        assert!(ran, "graphics walk started without a parked submitter");
    }
    pending().submit();
}

/// A message to the render thread: graphics, scanout, resize, or shutdown. One
/// enum so a single `recv` wakes on any of them — shutdown/resize don't depend on DL traffic.
pub(crate) enum RenderMsg {
    Gfx {
        data_ptr: usize,
        microcode: ultragfx::Microcode,
        sidecar: Option<crate::interpolation::Sidecar>,
        done: Sender<()>,
    },
    Scanout {
        address: u64,
        enqueued_at: std::time::Instant,
    },
    Resize {
        width: u32,
        height: u32,
    },
    DisplayRefreshRate(Option<u32>),
    Shutdown,
}

struct GfxChannel {
    tx: Sender<RenderMsg>,
    rx: Mutex<Option<Receiver<RenderMsg>>>,
}

fn gfx() -> &'static GfxChannel {
    static GFX: OnceLock<GfxChannel> = OnceLock::new();
    GFX.get_or_init(|| {
        let (tx, rx) = channel::<RenderMsg>();
        GfxChannel {
            tx,
            rx: Mutex::new(Some(rx)),
        }
    })
}

/// The render thread takes sole ownership of the receiver at spawn. If it later dies, the receiver
/// drops and every `tx.send` returns `Err` — a fast, controlled failure instead of an infinite block.
pub(crate) fn take_render_receiver() -> Receiver<RenderMsg> {
    gfx()
        .rx
        .lock()
        .unwrap()
        .take()
        .expect("render receiver already taken")
}

/// Submit a DL and block (run token held) until the render thread has walked it. `true` once walked,
/// `false` if the render thread is gone — the caller then skips completion so a dead consumer never
/// yields a false SP→DP. While blocked the caller is parked, so the render thread may complete the
/// previous task under its token.
pub(crate) fn submit_and_wait(
    data_ptr: usize,
    microcode: ultragfx::Microcode,
    sidecar: Option<crate::interpolation::Sidecar>,
) -> bool {
    let (done_tx, done_rx) = channel::<()>();
    pending().submit();
    super::sched::set_gfx_parked(true);
    let walked = gfx()
        .tx
        .send(RenderMsg::Gfx {
            data_ptr,
            microcode,
            sidecar,
            done: done_tx,
        })
        .is_ok()
        && done_rx.recv().is_ok();
    super::sched::set_gfx_parked(false);
    if !walked {
        pending().abandon();
    }
    walked
}

/// A dropped render receiver is harmless during shutdown.
pub(crate) fn send_render_control(msg: RenderMsg) {
    let _ = gfx().tx.send(msg);
}

/// osSpTaskStartGo: run the registered microcode handler, then post completion.
#[no_mangle]
pub extern "C" fn R64SpTaskStartGo(task: *mut c_void) {
    if task.is_null() {
        return;
    }
    let (ty, data_ptr, ucode_ptr) = unsafe { read_task(task) };
    let handler = rsp::lookup(ucode_ptr).unwrap_or_else(|| {
        log::error!("unregistered RSP task: type {ty}, ucode {ucode_ptr:#x}");
        std::process::abort();
    });
    match handler {
        Handler::Gfx(microcode) => {
            if submit_and_wait(
                data_ptr,
                microcode,
                crate::interpolation::take(task as usize),
            ) {
                event::post(OS_EVENT_SP);
                // Replay completes the task here, as before the split. Its clock steps only when
                // guest work drains, so a later DP moves the retraces relative to the game's
                // frames, and recorded input scripts and baselines no longer line up.
                if crate::gamepad::script::replaying() {
                    pending().wait_while_in_flight();
                    complete_ready();
                }
            }
        }
        Handler::AudioAcmd | Handler::AudioZelda => {
            static AUDIO: OnceLock<Mutex<NativeContext>> = OnceLock::new();
            static ZELDA: OnceLock<Mutex<NativeContext>> = OnceLock::new();
            let (context, microcode) = if handler == Handler::AudioAcmd {
                (&AUDIO, Microcode::Classic)
            } else {
                (&ZELDA, Microcode::Zelda)
            };
            let mut context = context
                .get_or_init(|| Mutex::new(NativeContext::new(microcode)))
                .lock()
                .unwrap();
            if let Err(error) = unsafe { context.task(task) } {
                log::error!("{handler:?} audio task rejected: {error}");
            }
            // The post can hand the token to a thread that starts the next audio task.
            drop(context);
            event::post(OS_EVENT_SP);
        }
        Handler::JpegNjpgdsp => {
            unsafe { super::njpgdsp::process(data_ptr as *const c_void) };
            event::post(OS_EVENT_SP);
        }
        Handler::Skip => {
            for &ev in skip_completion_events(ty) {
                event::post(ev);
            }
        }
    }
}

/// osSpTaskYielded → 0: the cooperative one-DL-per-frame consume is never actually interrupted.
#[no_mangle]
pub extern "C" fn R64SpTaskYielded(_task: *mut c_void) -> i32 {
    0
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(super) static WAITING: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);

    // The scheduler, event table and render receiver are process-global.
    pub(crate) fn isolated(name: &str, replay: bool) -> bool {
        if std::env::var("REALITY64_RCP_TEST").as_deref() == Ok(name) {
            return false;
        }
        let path = std::env::temp_dir().join(format!(
            "r64-{}-{}.input",
            name.replace("::", "-"),
            std::process::id()
        ));
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", name, "--nocapture"])
            .env("REALITY64_RCP_TEST", name)
            .env_remove("REALITY64_INPUT_RECORD")
            .env_remove("REALITY64_INPUT_REPLAY");
        if replay {
            std::fs::write(&path, "reality64-input 1 1\n0 0000 0 0\nend 1000\n").unwrap();
            command.env("REALITY64_INPUT_REPLAY", &path);
        }
        let output = command.output().unwrap();
        if replay {
            std::fs::remove_file(path).unwrap();
        }
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        true
    }

    #[test]
    fn read_task_preserves_data_and_ucode_host_pointers() {
        let task = OSTaskT {
            type_: M_GFXTASK,
            data_ptr: 0x1_dead_beef as *mut u64,
            ucode: 0x2_1234_5678 as *mut u64,
            ..unsafe { std::mem::zeroed() }
        };
        let fields = unsafe { read_task(&task as *const OSTaskT as *const c_void) };
        assert_eq!(fields, (M_GFXTASK, 0x1_dead_beef, 0x2_1234_5678));
    }

    static MEMORY: [std::sync::atomic::AtomicU8; 8] =
        [const { std::sync::atomic::AtomicU8::new(0) }; 8];
    const DATA: usize = 0x1_dead_bee0;
    const SP_MSG: usize = 1;
    const DP_MSG: usize = 2;

    fn byte(n: usize) -> u8 {
        MEMORY[n].load(std::sync::atomic::Ordering::SeqCst)
    }

    struct TestConsumer {
        walked: Sender<(usize, ultragfx::Microcode)>,
        proceed: Receiver<()>,
        finish: Receiver<()>,
        finished: Sender<Sender<()>>,
    }

    impl crate::render::GfxConsumer for TestConsumer {
        fn consume_dl(
            &mut self,
            data_ptr: usize,
            microcode: ultragfx::Microcode,
            _: Option<crate::interpolation::Sidecar>,
            released: &mut dyn FnMut(),
        ) -> Vec<ultragfx::GuestWrite> {
            self.proceed.recv().unwrap();
            let n = data_ptr - DATA;
            if n > 1 {
                assert_eq!(
                    byte(n - 1),
                    (n - 1) as u8,
                    "task {n} walked over task {}'s unapplied patch",
                    n - 1
                );
            }
            self.walked.send((data_ptr, microcode)).unwrap();
            released();
            self.finish.recv().unwrap();
            vec![ultragfx::GuestWrite {
                address: MEMORY[n].as_ptr() as u64,
                bytes: vec![n as u8],
            }]
        }

        fn present(&mut self, _: Option<u64>) {
            let (resume, resumed) = channel();
            self.finished.send(resume).unwrap();
            resumed.recv().unwrap();
        }
        fn resize(&mut self, _: u32, _: u32) {}
    }

    struct Script {
        wait: usize,
        sent: usize,
        go: Mutex<Receiver<()>>,
    }

    fn start_gfx(n: usize) {
        let mut task = OSTaskT {
            type_: M_GFXTASK,
            data_ptr: (DATA + n) as *mut u64,
            ucode: 0x9234_5678 as *mut u64,
            ..unsafe { std::mem::zeroed() }
        };
        R64SpTaskStartGo(&mut task as *mut OSTaskT as *mut c_void);
    }

    extern "C" fn guest(arg: *mut c_void) {
        let s = unsafe { &*(arg as *const Script) };
        let go = || s.go.lock().unwrap().recv().unwrap();
        start_gfx(1);
        assert_eq!(super::super::mesg::recv(s.wait, 1).0, 0);
        start_gfx(2);
        go();
        start_gfx(3);
        start_gfx(4);
        go();
        assert_eq!(super::super::mesg::send(s.sent, 0, 0), 0);
        go();
        start_gfx(5);
        go();
        assert_eq!(super::super::mesg::recv(s.wait, 1).0, 0);
        start_gfx(6);
        go();
        assert_eq!(super::super::mesg::recv(s.wait, 0), (-1, 0));
        assert_eq!(byte(6), 6, "an empty nonblocking receive completes DP");
        go();
        start_gfx(7);
        go();
        let sched = super::super::sched::scheduler();
        sched.reschedule_yield(Some(super::super::sched::current_id()));
        assert_eq!(byte(7), 7, "a sole yielder completes DP");
    }

    fn queue(count: usize) -> usize {
        let backing: &'static mut [u8; 64] = Box::leak(Box::new([0; 64]));
        let mq = backing.as_mut_ptr() as *mut c_void;
        super::super::mesg::create(mq as usize, count as i32);
        mq as usize
    }

    fn poll(what: &str, mut done: impl FnMut() -> bool) {
        for _ in 0..2000 {
            if done() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("timed out waiting for {what}");
    }

    fn drain(events: usize) -> Vec<usize> {
        std::iter::from_fn(|| match super::super::mesg::recv(events, 0) {
            (0, m) => Some(m),
            _ => None,
        })
        .collect()
    }

    fn expect(events: usize, want: &[usize], site: &str) {
        let mut got = Vec::new();
        poll(site, || {
            got.extend(drain(events));
            got.len() >= want.len()
        });
        assert_eq!(got, want, "{site}");
    }

    #[test]
    fn gfx_tasks_complete_at_each_live_site_then_gone_returns_false() {
        use super::super::{event::R64EventSetMesg, mesg, sched, thread};
        if isolated(
            "ultra::rcp::tests::gfx_tasks_complete_at_each_live_site_then_gone_returns_false",
            false,
        ) {
            return;
        }
        let rx = take_render_receiver();
        let (walked_tx, walked) = channel();
        let (proceed, proceed_rx) = channel();
        let (finish, finish_rx) = channel();
        let (finished_tx, finished_rx) = channel();
        let render = std::thread::spawn(move || {
            crate::render::render_loop_on(
                &mut TestConsumer {
                    walked: walked_tx,
                    proceed: proceed_rx,
                    finish: finish_rx,
                    finished: finished_tx,
                },
                rx,
            );
        });
        let finish_task = || {
            finish.send(()).unwrap();
            finished_rx
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap()
        };
        let walk = || {
            proceed.send(()).unwrap();
            walked
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap()
        };
        let binding = rsp::R64RspBinding {
            ucode: 0x1_1234_5678 as *const c_void,
            handler: 4,
        };
        assert_eq!(rsp::R64RspRegister(&binding, 1), 0);
        let events = queue(16);
        R64EventSetMesg(OS_EVENT_SP, events as *mut c_void, SP_MSG as *mut c_void);
        R64EventSetMesg(OS_EVENT_DP, events as *mut c_void, DP_MSG as *mut c_void);
        let (go, go_rx) = channel();
        let script: &'static Script = Box::leak(Box::new(Script {
            wait: queue(1),
            sent: queue(1),
            go: Mutex::new(go_rx),
        }));
        let t: &'static mut [u8; 128] = Box::leak(Box::new([0; 128]));
        let tid = t.as_ptr() as usize;
        thread::R64ThreadCreate(
            t.as_mut_ptr() as *mut c_void,
            7,
            guest,
            script as *const Script as *mut c_void,
            std::ptr::null_mut(),
            10,
        );
        thread::R64ThreadStart(t.as_mut_ptr() as *mut c_void);
        let ready = || pending().is_ready();

        // Render thread, token free: the guest is blocked when task 1 finishes.
        assert_eq!(
            walk(),
            (DATA + 1, ultragfx::Microcode::S2dex2),
            "data_ptr and microcode round-trip intact"
        );
        poll("guest blocked", || sched::is_blocked(tid));
        expect(events, &[SP_MSG], "task 1 posts SP when walked");
        poll("guest released the token", || sched::run_as_host(|| {}));
        let resume = finish_task();
        expect(
            events,
            &[DP_MSG],
            "render thread completes task 1 on a free token",
        );
        assert_eq!(byte(1), 1);
        resume.send(()).unwrap();
        assert_eq!(mesg::send(script.wait, 0, 0), 0);

        // Render thread, holder parked, before the next walk.
        assert_eq!(walk().0, DATA + 2);
        expect(events, &[SP_MSG], "task 2 posts SP when walked");
        let resume = finish_task();
        poll("task 2 ready", ready);
        assert!(drain(events).is_empty(), "no DP while the guest runs");
        go.send(()).unwrap();
        poll("guest parked on task 3", sched::gfx_parked);
        resume.send(()).unwrap();
        assert_eq!(walk().0, DATA + 3);
        expect(
            events,
            &[DP_MSG, SP_MSG],
            "task 2 completes before task 3's walk",
        );

        // Render thread, holder parked, when the task finishes.
        poll("guest parked on task 4", sched::gfx_parked);
        let resume = finish_task();
        expect(
            events,
            &[DP_MSG],
            "task 3 completes at finish while the guest is parked",
        );
        resume.send(()).unwrap();
        assert_eq!(walk().0, DATA + 4);
        expect(events, &[SP_MSG], "task 4 posts SP when walked");

        // The guest's next send.
        let resume = finish_task();
        poll("task 4 ready", ready);
        assert!(drain(events).is_empty(), "no DP while the guest runs");
        go.send(()).unwrap();
        expect(events, &[DP_MSG], "the guest's send completes task 4");
        assert_eq!(byte(4), 4);
        resume.send(()).unwrap();

        // idle_step: the guest blocks with task 5 finished.
        go.send(()).unwrap();
        assert_eq!(walk().0, DATA + 5);
        expect(events, &[SP_MSG], "task 5 posts SP when walked");
        let resume = finish_task();
        poll("task 5 ready", ready);
        assert!(drain(events).is_empty(), "no DP while the guest runs");
        go.send(()).unwrap();
        expect(events, &[DP_MSG], "idle_step completes task 5");
        assert_eq!(byte(5), 5);
        resume.send(()).unwrap();
        assert_eq!(mesg::send(script.wait, 0, 0), 0);

        assert_eq!(walk().0, DATA + 6);
        expect(events, &[SP_MSG], "task 6 posts SP when walked");
        let resume = finish_task();
        poll("task 6 ready", ready);
        assert!(drain(events).is_empty());
        go.send(()).unwrap();
        expect(
            events,
            &[DP_MSG],
            "an empty nonblocking receive completes task 6",
        );
        resume.send(()).unwrap();

        go.send(()).unwrap();
        assert_eq!(walk().0, DATA + 7);
        expect(events, &[SP_MSG], "task 7 posts SP when walked");
        let resume = finish_task();
        poll("task 7 ready", ready);
        assert!(drain(events).is_empty());
        go.send(()).unwrap();
        expect(events, &[DP_MSG], "a sole yielder completes task 7");
        poll("guest exit", || {
            sched::TEST_EXITED.lock().unwrap().contains(&tid)
        });
        resume.send(()).unwrap();

        gfx().tx.send(RenderMsg::Shutdown).unwrap();
        render.join().unwrap();
        assert!(
            !submit_and_wait(0x1234, ultragfx::Microcode::F3d, None),
            "receiver dropped → render gone → false, no hang"
        );
    }

    #[test]
    fn replay_ready_at_sp_waits_for_the_receivers_turn_before_dp() {
        use super::super::{event::R64EventSetMesg, mesg, sched, thread};
        if isolated(
            "ultra::rcp::tests::replay_ready_at_sp_waits_for_the_receivers_turn_before_dp",
            true,
        ) {
            return;
        }

        struct ReadyConsumer(Sender<()>);
        impl crate::render::GfxConsumer for ReadyConsumer {
            fn consume_dl(
                &mut self,
                _: usize,
                _: ultragfx::Microcode,
                _: Option<crate::interpolation::Sidecar>,
                released: &mut dyn FnMut(),
            ) -> Vec<ultragfx::GuestWrite> {
                released();
                vec![ultragfx::GuestWrite {
                    address: MEMORY[1].as_ptr() as u64,
                    bytes: vec![1],
                }]
            }
            fn present(&mut self, _: Option<u64>) {
                self.0.send(()).unwrap();
            }
            fn resize(&mut self, _: u32, _: u32) {}
        }

        extern "C" fn receiver(arg: *mut c_void) {
            let events = arg as usize;
            assert_eq!(mesg::recv(events, 1), (0, 0));
            assert_eq!(mesg::recv(events, 1), (0, SP_MSG));
            assert_eq!(byte(1), 0, "SP reschedules before patches and DP");
            assert!(pending().is_ready());
            assert_eq!(mesg::recv(events, 0), (-1, 0));
            sched::scheduler().reschedule_yield(Some(sched::current_id()));
            assert_eq!(byte(1), 0, "polling and yielding cannot complete replay DP");
            assert_eq!(mesg::recv(events, 1), (0, DP_MSG));
            assert_eq!(byte(1), 1);
        }
        extern "C" fn submitter(_: *mut c_void) {
            start_gfx(1);
            assert_eq!(byte(1), 1);
        }

        let binding = rsp::R64RspBinding {
            ucode: 0x1_1234_5678 as *const c_void,
            handler: 4,
        };
        assert_eq!(rsp::R64RspRegister(&binding, 1), 0);
        let events = queue(4);
        R64EventSetMesg(OS_EVENT_SP, events as *mut c_void, SP_MSG as *mut c_void);
        R64EventSetMesg(OS_EVENT_DP, events as *mut c_void, DP_MSG as *mut c_void);
        let (finished, finished_rx) = channel();
        let render = std::thread::spawn(move || {
            crate::render::render_loop_on(&mut ReadyConsumer(finished), take_render_receiver());
        });
        let mut receiver_thread = [0u8; 128];
        let receiver_key = receiver_thread.as_mut_ptr() as usize;
        thread::R64ThreadCreate(
            receiver_key as *mut _,
            1,
            receiver,
            events as *mut _,
            std::ptr::null_mut(),
            20,
        );
        thread::R64ThreadStart(receiver_key as *mut _);
        poll("receiver blocked", || sched::is_blocked(receiver_key));
        let (locked, locked_rx) = channel();
        let (unlock, unlock_rx) = channel();
        let blocker = std::thread::spawn(move || {
            mesg::post_with(events, 0, || {
                locked.send(()).unwrap();
                unlock_rx.recv().unwrap();
            });
        });
        locked_rx.recv().unwrap();
        let mut submitter_thread = [0u8; 128];
        let submitter_key = submitter_thread.as_mut_ptr() as usize;
        thread::R64ThreadCreate(
            submitter_key as *mut _,
            2,
            submitter,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            10,
        );
        thread::R64ThreadStart(submitter_key as *mut _);
        finished_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        assert!(
            pending().is_ready(),
            "render finishes while the SP queue is locked"
        );
        unlock.send(()).unwrap();
        blocker.join().unwrap();
        poll("both replay threads exited", || {
            let exited = sched::TEST_EXITED.lock().unwrap();
            exited.contains(&receiver_key) && exited.contains(&submitter_key)
        });
        gfx().tx.send(RenderMsg::Shutdown).unwrap();
        render.join().unwrap();
    }

    #[test]
    fn graphics_memory_guards_stop_unsafe_apply_and_walk() {
        if isolated(
            "ultra::rcp::tests::graphics_memory_guards_stop_unsafe_apply_and_walk",
            false,
        ) {
            return;
        }
        let sched = super::super::sched::scheduler();
        sched.register(1, 10);
        sched.mark_ready(1);
        sched.reschedule(None);
        let patch = || {
            vec![ultragfx::GuestWrite {
                address: MEMORY[1].as_ptr() as u64,
                bytes: vec![1],
            }]
        };
        pending().finish(patch());
        assert!(std::panic::catch_unwind(complete_before_walk).is_err());
        assert!(pending().is_ready());
        assert_eq!(byte(1), 0);
        assert!(std::panic::catch_unwind(complete_ready).is_err());
        assert!(
            pending().is_ready(),
            "an unsafe apply must retain the patches"
        );
        assert_eq!(byte(1), 0);
        sched.set_gfx_parked(true);
        assert!(sched.run_as_host(|| {
            assert!(complete_ready());
        }));
        sched.set_gfx_parked(false);
        assert_eq!(byte(1), 1);
        sched.on_exit(1);
    }

    fn write(byte: u8) -> Vec<ultragfx::GuestWrite> {
        vec![ultragfx::GuestWrite {
            address: 0x1000,
            bytes: vec![byte],
        }]
    }

    fn bytes(writes: Option<Vec<ultragfx::GuestWrite>>) -> Option<Vec<u8>> {
        writes.map(|w| w.into_iter().flat_map(|w| w.bytes).collect())
    }

    #[test]
    fn a_submit_never_overwrites_a_task_in_flight_or_ready() {
        let pending = Pending::new();
        pending.submit();
        pending.submit();
        pending.finish(write(1));
        pending.submit();
        assert!(
            pending.is_ready(),
            "task 1's patches survive task 2's submit"
        );
        assert_eq!(bytes(pending.take_ready()), Some(vec![1]));
        assert_eq!(bytes(pending.take_ready()), None);
    }

    #[test]
    fn replay_waits_for_the_task_in_flight_then_takes_its_patches() {
        use super::super::{event::R64EventSetMesg, mesg, sched, thread};
        use std::sync::atomic::Ordering;
        if isolated(
            "ultra::rcp::tests::replay_waits_for_the_task_in_flight_then_takes_its_patches",
            true,
        ) {
            return;
        }
        extern "C" fn first(_: *mut c_void) {
            start_gfx(1);
            assert_eq!(byte(1), 1);
        }
        extern "C" fn second(arg: *mut c_void) {
            let events = arg as usize;
            assert_eq!(mesg::recv(events, 1), (0, SP_MSG));
            MEMORY[0].store(1, Ordering::SeqCst);
            start_gfx(2);
            assert_eq!(
                byte(2),
                2,
                "StartGo waits for the overlapping task's patches"
            );
            assert_eq!(drain(events), [DP_MSG, SP_MSG, DP_MSG]);
        }

        let binding = rsp::R64RspBinding {
            ucode: 0x1_1234_5678 as *const c_void,
            handler: 4,
        };
        assert_eq!(rsp::R64RspRegister(&binding, 1), 0);
        let events = queue(8);
        R64EventSetMesg(OS_EVENT_SP, events as *mut c_void, SP_MSG as *mut c_void);
        R64EventSetMesg(OS_EVENT_DP, events as *mut c_void, DP_MSG as *mut c_void);
        let (walked_tx, walked) = channel();
        let (proceed, proceed_rx) = channel();
        let (finish, finish_rx) = channel();
        let (finished_tx, finished_rx) = channel();
        let render = std::thread::spawn(move || {
            crate::render::render_loop_on(
                &mut TestConsumer {
                    walked: walked_tx,
                    proceed: proceed_rx,
                    finish: finish_rx,
                    finished: finished_tx,
                },
                take_render_receiver(),
            );
        });
        let mut high = [0u8; 128];
        let high_id = high.as_mut_ptr() as usize;
        thread::R64ThreadCreate(
            high_id as *mut _,
            2,
            second,
            events as *mut _,
            std::ptr::null_mut(),
            20,
        );
        thread::R64ThreadStart(high_id as *mut _);
        poll("second submitter blocked on SP", || {
            sched::is_blocked(high_id)
        });
        let mut low = [0u8; 128];
        let low_id = low.as_mut_ptr() as usize;
        thread::R64ThreadCreate(
            low_id as *mut _,
            1,
            first,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            10,
        );
        thread::R64ThreadStart(low_id as *mut _);
        proceed.send(()).unwrap();
        assert_eq!(
            walked
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap()
                .0,
            DATA + 1
        );
        poll("second submit overlaps the first finish", || {
            byte(0) == 1 && sched::gfx_parked()
        });
        finish.send(()).unwrap();
        finished_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap()
            .send(())
            .unwrap();
        proceed.send(()).unwrap();
        assert_eq!(
            walked
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap()
                .0,
            DATA + 2
        );
        poll("second submitter waits after release", || {
            WAITING.load(Ordering::SeqCst) != 0
        });
        assert_eq!(byte(1), 1);
        assert_eq!(byte(2), 0);
        finish.send(()).unwrap();
        let resume = finished_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        poll("overlapping replay submitters exited", || {
            let exited = sched::TEST_EXITED.lock().unwrap();
            exited.contains(&low_id) && exited.contains(&high_id)
        });
        resume.send(()).unwrap();
        gfx().tx.send(RenderMsg::Shutdown).unwrap();
        render.join().unwrap();
    }

    #[test]
    fn skip_completion_follows_task_type() {
        assert_eq!(
            skip_completion_events(M_GFXTASK),
            &[OS_EVENT_SP, OS_EVENT_DP]
        );
        for task_type in [2, 4, 99] {
            assert_eq!(skip_completion_events(task_type), &[OS_EVENT_SP]);
        }
    }

    #[test]
    fn ostask_layout_matches_sptask_h() {
        assert_eq!(std::mem::size_of::<OSTaskT>(), 0x78);
        assert_eq!(std::mem::offset_of!(OSTaskT, ucode_data), 0x28);
        assert_eq!(std::mem::offset_of!(OSTaskT, ucode_data_size), 0x30);
        assert_eq!(std::mem::offset_of!(OSTaskT, data_size), 0x60);
        assert_eq!(std::mem::offset_of!(OSTaskT, type_), 0);
        assert_eq!(std::mem::offset_of!(OSTaskT, ucode), 24);
        assert_eq!(std::mem::offset_of!(OSTaskT, data_ptr), 88);
    }
}
