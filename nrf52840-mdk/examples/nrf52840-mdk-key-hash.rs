#![no_main]
#![no_std]

#[allow(unused_imports)]
use panic_semihosting;

use cortex_m_semihosting::hprintln;
use rtfm::app;

use nrf52840_hal::{clocks, prelude::*};

use nrf52840_pac as pac;

/// Key length
pub const KEY_SIZE: usize = 16;

/// Cipher block length
pub const BLOCK_SIZE: usize = 16;

pub const ECB_BLOCK_SIZE: usize = KEY_SIZE + BLOCK_SIZE + BLOCK_SIZE;

#[derive(Clone, Debug, PartialEq)]
pub enum SecurityError {
    ResourceConflict,
}

pub struct Aes128Ecb {
    ecb: pac::ECB,
    buffer: [u8; ECB_BLOCK_SIZE],
}

impl Aes128Ecb {
    pub fn new(ecb: pac::ECB) -> Self {
        Self {
            ecb,
            buffer: [0u8; ECB_BLOCK_SIZE],
        }
    }

    pub fn set_key(&mut self, key: &[u8]) -> Result<(), SecurityError> {
        assert!(key.len() == KEY_SIZE);
        self.buffer[..KEY_SIZE].copy_from_slice(&key);
        Ok(())
    }

    pub fn process(&mut self, input: &[u8], output: &mut [u8]) -> Result<(), SecurityError> {
        assert!(input.len() == BLOCK_SIZE);
        assert!(output.len() == BLOCK_SIZE);
        self.buffer[KEY_SIZE..KEY_SIZE + BLOCK_SIZE].copy_from_slice(input);
        let data_ptr = &mut self.buffer as *mut _ as u32;
        self.ecb.ecbdataptr.write(|w| unsafe { w.bits(data_ptr) });
        self.ecb
            .tasks_startecb
            .write(|w| w.tasks_startecb().set_bit());
        loop {
            if self
                .ecb
                .events_errorecb
                .read()
                .events_errorecb()
                .bit_is_set()
            {
                return Err(SecurityError::ResourceConflict);
            }
            if self.ecb.events_endecb.read().events_endecb().bit_is_set() {
                output.copy_from_slice(&self.buffer[KEY_SIZE + BLOCK_SIZE..]);
                break;
            }
        }
        Ok(())
    }
}

pub struct SecurityService {
    cipher: Aes128Ecb,
}

impl SecurityService {
    pub fn new(cipher: Aes128Ecb) -> Self {
        Self { cipher }
    }

    /// Process a block for the Key-hash hash function
    fn hash_key_process_block(
        &mut self,
        input: &[u8],
        mut output: &mut [u8],
    ) -> Result<(), SecurityError> {
        self.cipher.set_key(&output)?;
        self.cipher.process(&input, &mut output)?;
        // XOR the input into the hash block
        for n in 0..BLOCK_SIZE {
            output[n] ^= input[n];
        }
        Ok(())
    }

    /// Key-hash hash function
    fn hash_key_hash(&mut self, input: &[u8], output: &mut [u8]) -> Result<(), SecurityError> {
        assert!(input.len() < 4096);

        // Clear the first block of output
        for b in output[..BLOCK_SIZE].iter_mut() {
            *b = 0;
        }

        let mut blocks = input.chunks_exact(BLOCK_SIZE);

        // Process input data in cipher block sized chunks
        loop {
            match blocks.next() {
                Some(input_block) => {
                    self.hash_key_process_block(&input_block, &mut output[..BLOCK_SIZE])?;
                }
                None => {
                    let mut block = [0u8; BLOCK_SIZE];
                    let remainder = blocks.remainder();
                    assert!(remainder.len() < BLOCK_SIZE - 3);
                    block[..remainder.len()].copy_from_slice(remainder);
                    block[remainder.len()] = 0x80;
                    let input_len = input.len() as u16 * 8;
                    // Append the data length to the end
                    block[BLOCK_SIZE - 2] = (input_len >> 8) as u8;
                    block[BLOCK_SIZE - 1] = (input_len & 0xff) as u8;
                    self.hash_key_process_block(&block, &mut output[..BLOCK_SIZE])?;
                    break;
                }
            }
        }
        Ok(())
    }

    /// FIPS Pub 198 HMAC
    pub fn hash_key(
        &mut self,
        key: &[u8; KEY_SIZE],
        input: u8,
        result: &mut [u8],
    ) -> Result<(), SecurityError> {
        const HASH_INNER_PAD: u8 = 0x36;
        const HASH_OUTER_PAD: u8 = 0x5c;
        let mut hash_in = [0; BLOCK_SIZE * 2];
        let mut hash_out = [0; BLOCK_SIZE + 1];
        {
            // XOR the key with the outer padding
            for n in 0..KEY_SIZE {
                hash_in[n] = key[n] ^ HASH_OUTER_PAD;
            }
            // XOR the key with the inner padding
            for n in 0..KEY_SIZE {
                hash_out[n] = key[n] ^ HASH_INNER_PAD;
            }
            // Append the input byte
            hash_out[BLOCK_SIZE] = input;
            // Hash hash_out to form (Key XOR opad) || H((Key XOR ipad) || text)
            self.hash_key_hash(&hash_out[..=BLOCK_SIZE], &mut hash_in[BLOCK_SIZE..])?;
            // Hash hash_in to get the result
            self.hash_key_hash(&hash_in, &mut hash_out)?;
        }
        {
            // Take the key
            let (output_key, _) = result.split_at_mut(KEY_SIZE);
            output_key.copy_from_slice(&hash_out[..KEY_SIZE]);
        }
        Ok(())
    }
}

/// Default link key, "ZigBeeAlliance09"
pub const DEFAULT_LINK_KEY: [u8; KEY_SIZE] = [
    0x5a, 0x69, 0x67, 0x42, 0x65, 0x65, 0x41, 0x6c, 0x6c, 0x69, 0x61, 0x6e, 0x63, 0x65, 0x30, 0x39,
];

#[app(device = nrf52840_pac)]
const APP: () = {
    static mut SECURITY_SERVICE: SecurityService = ();

    #[init]
    fn init() {
        // Configure to use external clocks, and start them
        let _clocks = device
            .CLOCK
            .constrain()
            .enable_ext_hfosc()
            .set_lfclk_src_external(clocks::LfOscConfiguration::NoExternalNoBypass)
            .start_lfclk();

        let aes128_ecb = Aes128Ecb::new(device.ECB);
        let security_service = SecurityService::new(aes128_ecb);

        SECURITY_SERVICE = security_service;
    }

    #[idle(resources = [SECURITY_SERVICE])]
    fn idle() -> ! {
        let mut security_service = resources.SECURITY_SERVICE;

        // C.6.1 Test Vector Set 1
        let key = [
            0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4A, 0x4B, 0x4C, 0x4D,
            0x4E, 0x4F,
        ];
        let mut calculated = [0; BLOCK_SIZE];

        security_service
            .hash_key(&key, 0xc0, &mut calculated)
            .unwrap();
        if calculated
            == [
                0x45, 0x12, 0x80, 0x7B, 0xF9, 0x4C, 0xB3, 0x40, 0x0F, 0x0E, 0x2C, 0x25, 0xFB, 0x76,
                0xE9, 0x99,
            ]
        {
            hprintln!("Test 1 succeded").unwrap();
        } else {
            hprintln!("Test 1 failed").unwrap();
        }

        security_service
            .hash_key(&DEFAULT_LINK_KEY, 0x00, &mut calculated)
            .unwrap();
        if calculated
            == [
                0x4b, 0xab, 0x0f, 0x17, 0x3e, 0x14, 0x34, 0xa2, 0xd5, 0x72, 0xe1, 0xc1, 0xef, 0x47,
                0x87, 0x82,
            ]
        {
            hprintln!("Test 2 succeded").unwrap();
        } else {
            hprintln!("Test 2 failed").unwrap();
        }

        loop {}
    }
};
