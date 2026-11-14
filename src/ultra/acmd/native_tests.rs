use super::*;

#[test]
fn tokens_preserve_full_addresses_residues_and_reject_modified_words() {
    let mut tokens = Tokens::default();
    for residue in 0..8 {
        let address = 0x1_1234_5600 + residue;
        let token = tokens.insert(address).unwrap();
        assert_eq!(token & 7, residue as u32);
        assert_eq!(tokens.insert(address).unwrap(), token);
        assert_eq!(tokens.address(token).unwrap(), address);
        assert!(tokens.address(token ^ 0x10000).is_err());
        assert!(tokens.address(token ^ 1).is_err());
    }
    assert!(tokens.address(0).is_err());
}

#[test]
fn data_load_converts_halfwords_without_clearing_carried_state() {
    let mut context = NativeContext::default();
    context.state.dmem.bytes.fill(0xa5);
    context.state.vco = 0x1234;
    context.load_data(&[0x12, 0x34, 0xab, 0xcd]);
    assert_eq!(context.state.dmem.h16(0), 0x1234);
    assert_eq!(context.state.dmem.word(0), 0x1234abcd);
    assert_eq!(context.state.dmem.raw(4), 0xa5);
    assert_eq!(context.state.vco, 0x1234);
}

unsafe fn run(context: &mut NativeContext, packets: &[[usize; 2]]) -> Result<(), String> {
    unsafe { context.commands(packets.as_ptr(), (packets.len() * 16) as u32) }
}

#[test]
fn zero_length_adpcm_load_fails_without_accessing_host_memory() {
    let mut context = NativeContext::default();
    let error = unsafe { run(&mut context, &[[0x0b000000, 8]]) }.unwrap_err();
    assert!(error.contains("DMA length underflow"));
}

#[test]
fn packets_use_low_immediates_full_pointers_and_ignore_segments() {
    let mut context = NativeContext::default();
    let mut source = [0x1122334455667788u64; 8];
    let mut dest = [0u64; 8];
    let input = source.as_mut_ptr() as usize;
    let output = dest.as_mut_ptr() as usize;
    if cfg!(target_pointer_width = "64") {
        assert!(input > u32::MAX as usize);
        assert!(output > u32::MAX as usize);
    }
    let packets = [
        [0x12345678_07000000, 0xdeadbeef_00200000],
        [0x12345678_08000243, 0xdeadbeef_03430009],
        [0x12345678_04000000, input + 3],
        [0x12345678_08000343, 0xdeadbeef_02430009],
        [0x12345678_86000000, output + 5],
    ];
    unsafe { run(&mut context, &packets) }.unwrap();
    assert_eq!(&dest[..2], &source[..2]);
    assert_eq!(dest[2], 0);
    assert_eq!(context.state.dmem.word(0x320), 0x200000);
    assert_eq!(context.state.dmem.h16(0x360), 0x903);
    assert_eq!(context.state.dmem.h16(0x362), 0x803);
    assert_eq!(context.state.dmem.h16(0x364), 9);
    assert_eq!(&context.state.dmem.bytes[0x380..0x4c0], &[0; 320]);
}

#[test]
fn host_dma_rounds_wraps_and_keeps_row_skips_unaligned() {
    let mut context = NativeContext::default();
    let mut source = [0u64; 32];
    let mut dest = [0xa5a5a5a5a5a5a5a5u64; 32];
    let input = source.as_mut_ptr().cast::<u8>();
    for i in 0..256 {
        unsafe { input.add(i).write(i as u8) };
    }
    let from = context.memory.tokens.insert(input as usize + 3).unwrap();
    let to = context
        .memory
        .tokens
        .insert(dest.as_mut_ptr() as usize + 7)
        .unwrap();
    let value = (5 << 20) | (2 << 12) | 9;
    context
        .memory
        .dma(&mut context.state.dmem, 0xffd, from, value, false)
        .unwrap();
    context
        .memory
        .dma(&mut context.state.dmem, 0xffb, to, value, true)
        .unwrap();
    let bytes = unsafe { std::slice::from_raw_parts(dest.as_ptr().cast::<u8>(), 256) };
    for row in 0..3 {
        for i in 0..16 {
            assert_eq!(bytes[row * 21 + i], (row * 21 + i) as u8);
            assert_eq!(
                context.state.dmem.raw((0xff8 + row * 16 + i) as u32),
                (row * 21 + i) as u8
            );
        }
        assert!(bytes[row * 21 + 16..row * 21 + 21]
            .iter()
            .all(|&v| v == 0xa5));
    }
}

#[test]
fn loop_token_survives_task_reload_and_setvol_can_alias_or_invalidate_it() {
    let mut context = NativeContext::default();
    let mut history = [0x1234123412341234u64; 6];
    let mut output = [0u64; 4];
    let pointer = history.as_mut_ptr() as usize + 3;
    unsafe { run(&mut context, &[[0x0f000000, pointer]]) }.unwrap();
    let token = context.state.dmem.word(0x370);
    assert_eq!(token & 7, 3);
    context.load_data(&[0; DATA_SIZE]);
    let decode = [
        [0x08000240, 0x03400000],
        [0x01020000, output.as_mut_ptr() as usize],
    ];
    unsafe { run(&mut context, &decode) }.unwrap();
    assert_eq!(context.state.dmem.h16(0x900), 0x1234);
    assert!(output.iter().all(|&v| v == 0x1234123412341234));
    unsafe {
        run(
            &mut context,
            &[[0x09020000 | (token >> 16) as usize, (token << 16) as usize]],
        )
    }
    .unwrap();
    assert_eq!(context.state.dmem.word(0x370), token);
    unsafe { run(&mut context, &decode) }.unwrap();
    unsafe { run(&mut context, &[[0x0902ffff, 0xffffffff]]) }.unwrap();
    assert_eq!(context.state.dmem.word(0x370), u32::MAX);
    assert!(unsafe { run(&mut context, &decode) }
        .unwrap_err()
        .contains("unmapped"));
}

#[test]
fn resample_scratch_retains_a_token_and_packet_count_uses_sixteen_bytes() {
    let mut context = NativeContext::default();
    let mut state = [0u64; 4];
    let packets = [
        [0x08000240, 0x03400000],
        [0x05010000, state.as_mut_ptr() as usize],
    ];
    unsafe { context.commands(packets.as_ptr(), 47) }.unwrap();
    let token = context.state.dmem.word(0xfd0);
    assert_eq!(
        context.memory.tokens.address(token).unwrap(),
        state.as_ptr() as usize
    );
    unsafe { context.commands(std::ptr::null(), 15) }.unwrap();
}

#[test]
fn task_rejects_wrong_extent_or_hash_before_executing_commands() {
    for (microcode, extent, task_size) in
        [(Microcode::Classic, 704, 704), (Microcode::Zelda, 736, 735)]
    {
        let mut context = NativeContext::new(microcode);
        let mut data = vec![0u64; extent / 8];
        let mut task = OSTaskT {
            ucode_data: data.as_mut_ptr(),
            ucode_data_size: task_size,
            data_ptr: std::ptr::dangling_mut(),
            data_size: 16,
            ..OSTaskT::default()
        };
        let error = unsafe { context.task((&task as *const OSTaskT).cast()) }.unwrap_err();
        assert!(error.contains("SHA-256"));
        task.ucode_data_size -= 1;
        task.ucode_data = std::ptr::dangling_mut();
        let error = unsafe { context.task((&task as *const OSTaskT).cast()) }.unwrap_err();
        assert!(error.contains(&format!("{extent} raw bytes, task size {task_size}")));
    }
}

#[test]
fn zelda_loop_history_survives_reload_and_signed_bytes_stay_raw() {
    let mut context = NativeContext::new(Microcode::Zelda);
    let mut history = [0x1234abcd5678ef90u64; 4];
    let mut output = [0u64; 4];
    unsafe { run(&mut context, &[[0x0f000000, history.as_mut_ptr() as usize]]) }.unwrap();
    context.load_data(&[0; 736]);
    unsafe {
        run(
            &mut context,
            &[
                [0x08000500, 0x09000000],
                [0x17020000, output.as_mut_ptr() as usize],
            ],
        )
    }
    .unwrap();
    assert_eq!(output, history);

    let samples = [0x80, 0x7f, 0xff, 1, 0, 0x40, 0xc0, 3];
    for i in 0..32 {
        context
            .state
            .dmem
            .set_raw(0x500 + i, samples[i as usize % 8]);
    }
    unsafe {
        run(
            &mut context,
            &[
                [0x08000500, 0x09000020],
                [0x17010000, output.as_mut_ptr() as usize],
            ],
        )
    }
    .unwrap();
    let actual = unsafe { std::slice::from_raw_parts(output.as_ptr().cast::<i16>(), 16) };
    assert_eq!(
        actual,
        &[-32768, 32512, -256, 256, 0, 16384, -16384, 768].repeat(2)
    );
}

#[test]
fn zelda_diagnostics_count_console_commands_across_native_refills() {
    let mut context = NativeContext::new(Microcode::Zelda);
    let mut output = [0u64; 2];
    let mut packets = [[0x08000500, 0x09000000]; 10];
    packets[8] = [0x00011234, output.as_mut_ptr() as usize];
    unsafe { run(&mut context, &packets) }.unwrap();
    let actual = unsafe { std::slice::from_raw_parts(output.as_ptr().cast::<u16>(), 8) };
    assert_eq!(actual, &[0x1234, 0, 0, 1, 0, 8, 0, 8]);
}

#[test]
fn zelda_task_start_resets_the_fallthrough_sample_phase() {
    let mut context = NativeContext::new(Microcode::Zelda);
    context.state.dmem.set_h16(0, 0x1111);
    context.state.dmem.set_h16(2, 0x2222);
    for _ in 0..2 {
        context.state.scalar[12] = 0x7e00;
        context.state.scalar[13] = 16;
        context.state.scalar[15] = 0x900;
        unsafe { run(&mut context, &[[0x19000000, 0]]) }.unwrap();
        assert_eq!(context.state.dmem.h16(0x910), 0x2222);
    }
}

#[test]
fn zelda_rejects_overwritten_dispatch_before_the_next_command() {
    for opcode in [0x19000000, 0x02000020] {
        let mut context = NativeContext::new(Microcode::Zelda);
        context.state.dmem.set_h16(0x20, 0x1234);
        context.state.scalar[15] = 0x20;
        let error =
            unsafe { run(&mut context, &[[opcode, 16], [0x08000500, 0x09000020]]) }.unwrap_err();
        assert!(error.contains("overwritten Zelda audio dispatch entry 0x8"));
        assert_eq!(context.state.dmem.h16(0x2e0), 0);
    }
}

#[test]
fn zelda_zero_dma_counts_fail_without_accessing_host_memory() {
    let mut context = NativeContext::new(Microcode::Zelda);
    for opcode in [0x00000000, 0x0b000000, 0x14000000, 0x15000000] {
        let error = unsafe { run(&mut context, &[[opcode, 8]]) }.unwrap_err();
        assert!(error.contains("DMA length underflow"));
    }
}

#[test]
fn zelda_load_buffer_below_sixteen_bytes_is_the_refused_underflow() {
    let source = [0x1122334455667788u64; 2];
    let address = source.as_ptr() as usize;
    let load = |size: usize| (20 << 24) | ((size >> 4) << 16) | 0x5c0;
    let mut context = NativeContext::new(Microcode::Zelda);
    let error = unsafe { run(&mut context, &[[load(15), address]]) }.unwrap_err();
    assert!(error.contains("DMA length underflow"));
    assert_eq!(context.state.dmem.bytes[0x5c0..0x5d0], [0; 16]);
    unsafe { run(&mut context, &[[load(16), address]]) }.unwrap();
    assert!(!context.state.dmem.bytes[0x5c0..0x5d0].contains(&0));
    assert_eq!(context.state.dmem.bytes[0x5d0], 0);
}

#[test]
fn zelda_addmixer_rounds_to_sixty_four_bytes_and_consumes_carry_once() {
    let mut context = NativeContext::new(Microcode::Zelda);
    let mut output = [0xa5a5a5a5a5a5a5a5u64; 10];
    for i in 0..32 {
        context.state.dmem.set_h16(0x500 + i * 2, i as u16 * 100);
        context.state.dmem.set_h16(0x600 + i * 2, i as u16 * 200);
    }
    context.state.dmem.set_h16(0x500, 32760);
    context.state.dmem.set_h16(0x600, 20);
    context.state.dmem.set_h16(0x502, (-32760i16) as u16);
    context.state.dmem.set_h16(0x602, (-20i16) as u16);
    context.state.vectors[31] = [65535, 0, 65535, 0, 65535, 0, 65535, 0];
    unsafe {
        run(
            &mut context,
            &[
                [0x04010000, 0x05000600],
                [0x15040600, output.as_mut_ptr() as usize],
            ],
        )
    }
    .unwrap();
    let actual = unsafe { std::slice::from_raw_parts(output.as_ptr().cast::<i16>(), 32) };
    assert_eq!(&actual[..2], &[32767, -32768]);
    for (i, &sample) in actual.iter().enumerate().skip(2) {
        assert_eq!(sample, (i * 300 + usize::from(i < 8 && i % 2 == 0)) as i16);
    }
    assert_eq!(&output[8..], &[0xa5a5a5a5a5a5a5a5; 2]);
}

#[test]
fn packets_are_read_in_place_after_a_dma_overwrites_the_next_command() {
    let mut context = NativeContext::default();
    let mut packets = [
        [0x08000000usize, 0x02400010],
        [0x06000000, 0],
        [0x7f000000, 0],
    ];
    packets[1][1] = packets.as_mut_ptr() as usize + 32;
    for (i, value) in [0x09061234usize, 0].into_iter().enumerate() {
        context.state.dmem.bytes[0x800 + i * 8..0x808 + i * 8]
            .copy_from_slice(&value.to_ne_bytes());
    }
    unsafe { context.commands(packets.as_ptr(), 48) }.unwrap();
    assert_eq!(context.state.dmem.h16(0x366), 0x1234);
}
