use ivmp_core::{log, memory};

const GAME_MODE: usize = 0x011D_6FC8;
const NETWORK_GAME_CONFIG: (&str, usize) =
    ("B9 1E 00 00 00 BF ?? ?? ?? ?? F3 A5 5F 5E C3", 6);
const CONFIG_ZEROING_CALL: (&str, usize, usize) =
    ("6A 08 6A 3B FF 15 ?? ?? ?? ?? E8", 20, 0x76);

pub fn install() {
    unsafe {
        *(memory::from_ghidra(GAME_MODE) as *mut u32) = 1;
        if let Some(site) = memory::find(NETWORK_GAME_CONFIG.0) {
            let config =
                std::ptr::read_unaligned((site + NETWORK_GAME_CONFIG.1) as *const u32) as *mut u32;
            std::ptr::write_bytes(config, 0, 30);
            *config = 16;
            *config.add(2) = 32;
            log!("boot: network game config set to Free Mode, 32 slots");
            ivmp_networking::net::set_network_config(config as usize);
        }
        if let Some(site) = memory::find(CONFIG_ZEROING_CALL.0) {
            let call = site + CONFIG_ZEROING_CALL.1;
            let rel = std::ptr::read_unaligned((call + 1) as *const i32);
            let zeroing = (call as isize + 5 + rel as isize) as usize + CONFIG_ZEROING_CALL.2;
            if *(zeroing as *const u8) == 0xE8 {
                let _ = memory::patch(zeroing, &[0x90; 5]);
            }
        }
    }
}
