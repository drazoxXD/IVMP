//! Inline hooks for 32-bit code: overwrites the start of a function with a jump to ours and builds
//! a trampoline that runs the instructions it displaced and continues in the original. The
//! displaced instructions are decoded (iced-x86) and re-encoded at their new address, so relative
//! calls and jumps in them keep working and the hook works for any prologue, not just known ones.
//!
//! Use [`install`] on code in this process; [`relocate`] is the pure part (tested).

use iced_x86::{BlockEncoder, BlockEncoderOptions, Code, Decoder, DecoderOptions, Instruction, InstructionBlock};

/// Bytes of the jump written at the target (`jmp rel32`).
const JUMP_LEN: usize = 5;

/// An installed hook.
pub struct Hook {
    /// Calls the original function (runs the displaced instructions, then continues after them).
    pub trampoline: usize,
    /// The patched address.
    pub target: usize,
    /// How many bytes at the target were displaced.
    pub displaced: usize,
}

/// What a hook at `ip` needs: how many bytes it displaces (whole instructions covering at least
/// the 5 byte jump) and the trampoline code, to be placed at `trampoline_ip`.
pub fn relocate(code: &[u8], ip: u32, trampoline_ip: u32) -> Result<(usize, Vec<u8>), String> {
    let mut decoder = Decoder::with_ip(32, code, ip as u64, DecoderOptions::NONE);
    let mut instructions = Vec::new();
    let mut taken = 0usize;
    while taken < JUMP_LEN {
        if !decoder.can_decode() {
            return Err("not enough code to hook".into());
        }
        let instruction = decoder.decode();
        if instruction.is_invalid() {
            return Err(format!("cannot decode the instruction at {:#x}", ip as usize + taken));
        }
        taken += instruction.len();
        instructions.push(instruction);
        // Stop at an unconditional end of flow: nothing after it belongs to the function start.
        if taken < JUMP_LEN && matches!(instruction.code(), Code::Retnd | Code::Retnd_imm16) {
            return Err("the function is too short to hook (it returns before 5 bytes)".into());
        }
    }
    // Continue after the displaced instructions.
    instructions.push(Instruction::with_branch(Code::Jmp_rel32_32, ip as u64 + taken as u64).map_err(|e| e.to_string())?);
    let block = InstructionBlock::new(&instructions, trampoline_ip as u64);
    let encoded = BlockEncoder::encode(32, block, BlockEncoderOptions::NONE).map_err(|e| e.to_string())?;
    Ok((taken, encoded.code_buffer))
}

/// Hooks the function at `target`: calls to it go to `hook`, and the returned trampoline calls the
/// original. Same calling convention and arguments as the original, of course.
///
/// # Safety
/// `target` must be code in this process that nobody is executing the first bytes of right now (or
/// at least can tolerate the patch), and `hook` must be a function with the original's signature.
pub unsafe fn install(target: usize, hook: usize) -> Result<Hook, String> {
    use windows::Win32::System::Memory::{MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READWRITE, VirtualAlloc};

    let mut code = [0u8; 32];
    if !crate::memory::read(target, &mut code) {
        return Err(format!("cannot read the code at {target:#x}"));
    }
    let stub = unsafe { VirtualAlloc(None, 64, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE) } as usize;
    if stub == 0 {
        return Err("cannot allocate the trampoline".into());
    }
    let (displaced, trampoline_code) = relocate(&code, target as u32, stub as u32)?;
    if trampoline_code.len() > 64 {
        return Err("the trampoline is too large".into());
    }
    unsafe { std::ptr::copy_nonoverlapping(trampoline_code.as_ptr(), stub as *mut u8, trampoline_code.len()) };
    // jmp rel32 to the hook, padded with NOPs over the rest of the displaced instructions.
    let mut patch = vec![0x90u8; displaced];
    patch[0] = 0xE9;
    patch[1..JUMP_LEN].copy_from_slice(&(hook.wrapping_sub(target + JUMP_LEN) as u32).to_le_bytes());
    unsafe { crate::memory::patch(target, &patch) }.map_err(|e| e.to_string())?;
    Ok(Hook { trampoline: stub, target, displaced })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_prologue_is_copied_and_continued() {
        // mov edi, edi; push ebp; mov ebp, esp; sub esp, 8
        let code = [0x8B, 0xFF, 0x55, 0x8B, 0xEC, 0x83, 0xEC, 0x08, 0x90, 0x90];
        let (displaced, trampoline) = relocate(&code, 0x1000, 0x9000).unwrap();
        assert_eq!(displaced, 5);
        assert_eq!(&trampoline[..5], &code[..5]);
        // jmp 0x1005 from 0x9005
        assert_eq!(trampoline[5], 0xE9);
        let rel = i32::from_le_bytes(trampoline[6..10].try_into().unwrap());
        assert_eq!(0x9005i64 + 5 + rel as i64, 0x1005);
    }

    #[test]
    fn instruction_that_straddles_the_jump_is_displaced_whole() {
        // push 0x14; mov eax, imm32 (7 bytes in total, the second instruction crosses byte 5)
        let code = [0x6A, 0x14, 0xB8, 0x89, 0xDE, 0xAD, 0xBE, 0x90, 0x90, 0x90];
        let (displaced, _) = relocate(&code, 0x2000, 0x9000).unwrap();
        assert_eq!(displaced, 7);
    }

    #[test]
    fn relative_call_is_relocated() {
        // call 0x1100 (at 0x1000), then three nops
        let rel = 0x1100i32 - (0x1000 + 5);
        let mut code = vec![0xE8];
        code.extend_from_slice(&rel.to_le_bytes());
        code.extend_from_slice(&[0x90, 0x90, 0x90]);
        let (displaced, trampoline) = relocate(&code, 0x1000, 0x9000).unwrap();
        assert_eq!(displaced, 5);
        assert_eq!(trampoline[0], 0xE8);
        let moved = i32::from_le_bytes(trampoline[1..5].try_into().unwrap());
        assert_eq!(0x9000i64 + 5 + moved as i64, 0x1100, "the call still goes to the same function");
    }

    #[test]
    fn existing_jump_hook_is_chained() {
        // another hook's jmp 0x5000 at the start
        let rel = 0x5000i32 - (0x1000 + 5);
        let mut code = vec![0xE9];
        code.extend_from_slice(&rel.to_le_bytes());
        code.extend_from_slice(&[0x90; 8]);
        let (displaced, trampoline) = relocate(&code, 0x1000, 0x9000).unwrap();
        assert_eq!(displaced, 5);
        assert_eq!(trampoline[0], 0xE9);
        let moved = i32::from_le_bytes(trampoline[1..5].try_into().unwrap());
        assert_eq!(0x9000i64 + 5 + moved as i64, 0x5000);
    }

    #[test]
    fn too_short_function_is_refused() {
        // ret right away
        let code = [0xC3, 0x90, 0x90, 0x90, 0x90, 0x90];
        assert!(relocate(&code, 0x1000, 0x9000).is_err());
    }
}

/// The 4-byte fields in the game's code that hold an address in `range`: memory displacements and
/// 32-bit immediates of instructions, found by decoding the code (a plain search of the bytes for
/// the numbers finds many that are no operands, bytes of other instructions that happen to match).
/// Each is the address of the field itself, to be patched.
pub fn find_address_operands(range: std::ops::Range<usize>) -> Vec<usize> {
    let mut found = Vec::new();
    for (start, bytes) in crate::memory::snapshot(crate::memory::code_section()) {
        let mut decoder = Decoder::with_ip(32, &bytes, start as u64, DecoderOptions::NONE);
        let mut instruction = Instruction::default();
        while decoder.can_decode() {
            decoder.decode_out(&mut instruction);
            if instruction.is_invalid() {
                continue;
            }
            let offsets = decoder.get_constant_offsets(&instruction);
            let ip = instruction.ip() as usize;
            let mut fields = Vec::new();
            if offsets.has_displacement() && offsets.displacement_size() == 4 {
                fields.push(ip + offsets.displacement_offset());
            }
            if offsets.has_immediate() && offsets.immediate_size() == 4 {
                fields.push(ip + offsets.immediate_offset());
            }
            for field in fields {
                let at = field - start;
                if at + 4 <= bytes.len() {
                    let value = u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as usize;
                    if range.contains(&value) {
                        found.push(field);
                    }
                }
            }
        }
    }
    found
}
