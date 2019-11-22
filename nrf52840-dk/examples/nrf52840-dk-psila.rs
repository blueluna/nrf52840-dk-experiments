#![no_main]
#![no_std]

#[allow(unused_imports)]
use panic_itm;

use cortex_m::peripheral::ITM;

use rtfm::app;

use nrf52840_hal::{clocks, prelude::*};

use nrf52840_pac as pac;

use bbqueue::{self, bbq, BBQueue};

use log;

use nrf52_cryptocell::CryptoCellBackend;
use nrf52_radio_802154::{
    radio::{Radio, MAX_PACKET_LENGHT},
    timer::Timer,
};
use nrf52_utils::logger;
use psila_data::{security::DEFAULT_LINK_KEY, ExtendedAddress, Key};
use psila_service::{self, PsilaService};

#[app(device = nrf52840_pac, peripherals = true)]
const APP: () = {
    struct Resources {
        timer: pac::TIMER1,
        radio: Radio,
        service: PsilaService<CryptoCellBackend>,
        itm: ITM,
        rx_producer: bbqueue::Producer,
        rx_consumer: bbqueue::Consumer,
        tx_consumer: bbqueue::Consumer,
        log_consumer: bbqueue::Consumer,
    }

    #[init]
    fn init(cx: init::Context) -> init::LateResources {
        let log_consumer = logger::init();

        // Configure to use external clocks, and start them
        let _clocks = cx
            .device
            .CLOCK
            .constrain()
            .enable_ext_hfosc()
            .set_lfclk_src_external(clocks::LfOscConfiguration::NoExternalNoBypass)
            .start_lfclk();

        // MAC (EUI-48) address to EUI-64
        // Add FF FE in the middle
        //
        //    01 23 45 67 89 AB
        //  /  /  /       \  \  \
        // 01 23 45 FF FE 67 89 AB
        let devaddr_lo = cx.device.FICR.deviceaddr[0].read().bits();
        let devaddr_hi = cx.device.FICR.deviceaddr[1].read().bits() as u16;
        let extended_address = u64::from(devaddr_hi) << 48
            | u64::from(devaddr_lo & 0xff00_0000) << 40
            | u64::from(devaddr_lo & 0x00ff_ffff)
            | 0x0000_00ff_fe00_0000u64;
        let extended_address = ExtendedAddress::new(extended_address);

        let mut timer1 = cx.device.TIMER1;
        timer1.init();
        timer1.fire_at(1, 30_000_000);

        let mut radio = Radio::new(cx.device.RADIO);
        radio.set_channel(11);
        radio.set_transmission_power(8);
        radio.receive_prepare();

        let rx_queue = bbq![MAX_PACKET_LENGHT * 32].unwrap();
        let (rx_producer, rx_consumer) = rx_queue.split();

        let tx_queue = bbq![MAX_PACKET_LENGHT * 8].unwrap();
        let (tx_producer, tx_consumer) = tx_queue.split();

        let cryptocell = CryptoCellBackend::new(cx.device.CRYPTOCELL);
        let default_link_key = Key::from(DEFAULT_LINK_KEY);

        init::LateResources {
            timer: timer1,
            radio,
            service: PsilaService::new(cryptocell, tx_producer, extended_address, default_link_key),
            itm: cx.core.ITM,
            rx_producer,
            rx_consumer,
            tx_consumer,
            log_consumer,
        }
    }

    #[task(binds = TIMER1, resources = [service, timer], spawn = [radio_tx])]
    fn timer(cx: timer::Context) {
        let timer = cx.resources.timer;
        let service = cx.resources.service;

        log::info!("TIMER");

        timer.ack_compare_event(1);

        let fire_at = match service.timeout() {
            Ok(time) => time,
            Err(_) => {
                log::warn!("service timeout failed");
                0
            }
        };
        if fire_at > 0 {
            timer.fire_at(1, fire_at);
        }
        let _ = cx.spawn.radio_tx();
    }

    #[task(binds = RADIO, resources = [radio, service, rx_producer], spawn = [radio_tx])]
    fn radio(cx: radio::Context) {
        let mut packet = [0u8; MAX_PACKET_LENGHT as usize];
        let radio = cx.resources.radio;
        let service = cx.resources.service;
        let queue = cx.resources.rx_producer;

        let packet_len = radio.receive(&mut packet);
        if packet_len > 0 {
            match service.handle_acknowledge(&packet[1..packet_len - 1]) {
                Ok(to_me) => {
                    if to_me {
                        if let Ok(mut grant) = queue.grant(packet_len) {
                            grant.copy_from_slice(&packet[..packet_len]);
                            queue.commit(packet_len, grant);
                        }
                    }
                }
                Err(e) => {
                    log::warn!("service handle acknowledge failed, {:?}", e);
                }
            }
            let _ = cx.spawn.radio_tx();
        }
    }

    #[task(priority=1, resources = [rx_consumer, service, timer])]
    fn radio_rx(cx: radio_rx::Context) {
        let queue = cx.resources.rx_consumer;
        let service = cx.resources.service;
        let timer = cx.resources.timer;

        if let Ok(grant) = queue.read() {
            let packet_length = grant[0] as usize;
            let fire_at = match service.receive(&grant[1..packet_length - 1]) {
                Ok(fire_at) => fire_at,
                Err(e) => {
                    log::warn!("service receive failed, {:?}", e);
                    0
                }
            };
            if fire_at > 0 {
                timer.fire_at(1, fire_at);
            }
            queue.release(packet_length, grant);
        }
    }

    #[task(resources = [radio, tx_consumer], spawn = [radio_rx])]
    fn radio_tx(cx: radio_tx::Context) {
        let queue = cx.resources.tx_consumer;
        let radio = cx.resources.radio;

        if let Ok(grant) = queue.read() {
            let packet_length = grant[0] as usize;
            log::info!("Send {} octets", packet_length);
            let _ = radio.queue_transmission(&grant[1..=packet_length]);
            queue.release(packet_length + 1, grant);
        }
        let _ = cx.spawn.radio_rx();
    }

    #[idle(resources = [log_consumer, itm])]
    fn idle(cx: idle::Context) -> ! {
        let itm_port = &mut cx.resources.itm.stim[0];
        loop {
            while let Ok(grant) = cx.resources.log_consumer.read() {
                for chunk in grant.buf().chunks(256) {
                    cortex_m::itm::write_all(itm_port, chunk);
                }
                cx.resources.log_consumer.release(grant.buf().len(), grant);
            }
        }
    }

    extern "C" {
        fn PDM();
        fn QDEC();
    }
};