use std::io::{self, Read};
use std::time::Duration;

use clap::{App, AppSettings, Arg};

use serialport::prelude::*;

use slice_deque::SliceDeque;

use esercom;
use ieee802154::{beacon::Beacon, mac, mac_command};

fn parse_packet(packet: &[u8]) {
    use mac::Address;
    match mac::Frame::decode(packet) {
        Ok(p) => {
            print!("Packet",);
            match p.header.frame_type {
                mac::FrameType::Acknowledgement => {
                    print!(" TYPE: Acknowledgement");
                }
                mac::FrameType::Beacon => {
                    print!(" TYPE: Beacon");
                }
                mac::FrameType::Data => {
                    print!(" TYPE: Data");
                }
                mac::FrameType::MacCommand => {
                    print!(" TYPE: Command");
                }
            }
            print!("{}", if p.header.frame_pending { " PEND" } else { "" });
            print!("{}", if p.header.ack_request { " ACK" } else { "" });
            print!(
                "{}",
                if p.header.pan_id_compress {
                    " CMPR"
                } else {
                    ""
                }
            );
            print!(" SEQ: {}", p.header.seq);
            match p.header.destination {
                Address::Short(i, a) => {
                    print!(" DST: {:04x}:{:04x}", i.0, a.0);
                }
                Address::Extended(i, a) => {
                    print!(" DST: {:04x}:{:016x}", i.0, a.0);
                }
                Address::None => {
                    print!(" DST: None");
                }
            }
            match p.header.source {
                Address::Short(i, a) => {
                    print!(" SRC: {:04x}:{:04x}", i.0, a.0);
                }
                Address::Extended(i, a) => {
                    print!(" SRC: {:04x}:{:016x}", i.0, a.0);
                }
                Address::None => {
                    print!(" SRC: None");
                }
            }
            match p.header.frame_type {
                mac::FrameType::Acknowledgement => {
                    // Nothing here
                }
                mac::FrameType::Beacon => match Beacon::decode(p.payload) {
                    Ok((beacon, _)) => {
                        print!(
                            " Beacon: {:?} {:?} {} {} {} {} {} {} {} {}",
                            beacon.superframe_spec.beacon_order,
                            beacon.superframe_spec.superframe_order,
                            beacon.superframe_spec.final_cap_slot,
                            beacon.superframe_spec.battery_life_extension,
                            beacon.superframe_spec.pan_coordinator,
                            beacon.superframe_spec.association_permit,
                            beacon.guaranteed_time_slot_info.permit,
                            beacon.guaranteed_time_slot_info.slots().len(),
                            beacon.pending_address.short_addresses().len(),
                            beacon.pending_address.extended_addresses().len(),
                        );
                    }
                    Err(_) => {
                        print!(" Beacon: Failed to decode");
                    }
                },
                mac::FrameType::Data => {
                    // TODO: Parse data at higher layer?
                    print!(" Payload: ");
                    for b in p.payload {
                        print!("{:02x}", b);
                    }
                }
                mac::FrameType::MacCommand => {
                    if p.payload.len() > 0 {
                        match mac_command::Command::decode(p.payload) {
                            Ok((command, _)) => {
                                print!(" Command {:?}", command);
                            }
                            Err(_) => {
                                print!(" Command: Failed to decode");
                            }
                        }
                    } else {
                        print!(" No payload");
                    }
                }
            }
            println!("");
        }
        Err(e) => {
            println!("Unknown Packet");
            match e {
                mac::DecodeError::NotEnoughBytes => {
                    println!("NotEnoughBytes");
                }
                mac::DecodeError::InvalidFrameType(_) => {
                    println!("InvalidFrameType");
                }
                mac::DecodeError::SecurityNotSupported => {
                    println!("SecurityNotSupported");
                }
                mac::DecodeError::InvalidAddressMode(_) => {
                    println!("Invalid Address Mode");
                }
                mac::DecodeError::AddressModeNotSupported(am) => {
                    println!("AddressModeNotSupported");
                    match am {
                        mac::AddressMode::None => {
                            println!("Address Mode: None");
                        }
                        mac::AddressMode::Short => {
                            println!("Address Mode: Short");
                        }
                        mac::AddressMode::Extended => {
                            println!("Address Mode: Extended");
                        }
                    }
                }
                mac::DecodeError::InvalidFrameVersion(_) => {
                    println!("InvalidFrameVersion");
                }
                mac::DecodeError::InvalidValue => {
                    println!("InvalidValue");
                }
            }
        }
    }
}

fn main() {
    let matches = App::new("nRF52840-DK host companion")
        .about("Write stuff")
        .setting(AppSettings::DisableVersion)
        .arg(
            Arg::with_name("port")
                .help("The device path to a serial port")
                .use_delimiter(false)
                .required(true),
        )
        .get_matches();

    let port_name = matches.value_of("port").unwrap();
    let mut settings: SerialPortSettings = Default::default();
    settings.baud_rate = 115200;
    settings.timeout = Duration::from_millis(1000);

    let mut buffer: SliceDeque<u8> = SliceDeque::with_capacity(256);
    let mut data = [0u8; 256];
    let mut pkt_data = [0u8; 256];

    match serialport::open_with_settings(&port_name, &settings) {
        Ok(mut port) => {
            println!("Read packets over {}", &port_name);
            loop {
                match port.read(&mut data) {
                    Ok(rx_count) => {
                        buffer.extend_from_slice(&data[..rx_count]);
                        loop {
                            match esercom::com_decode(buffer.as_slice(), &mut data) {
                                Ok((msg, used, written)) => {
                                    if written == 0 {
                                        break;
                                    }
                                    match msg {
                                        esercom::MessageType::RadioReceive => {
                                            let pkt_len = written;
                                            let link_quality_indicator = data[pkt_len - 1];
                                            let pkt_len = pkt_len - 1; // Remove LQI
                                            pkt_data[..pkt_len].copy_from_slice(&data[..pkt_len]);
                                            // Add two dummy bytes which will be decoded as FCS
                                            pkt_data[pkt_len] = 0;
                                            pkt_data[pkt_len + 1] = 0;
                                            let pkt_len = pkt_len + 2; // Added FCS
                                            println!(
                                                "## Packet {} LQI {}",
                                                pkt_len, link_quality_indicator
                                            );
                                            for b in &pkt_data[..(pkt_len - 2)] {
                                                print!("{:02x}", b);
                                            }
                                            println!("");
                                            parse_packet(&pkt_data[..pkt_len]);
                                        }
                                        _ => println!("Other packet {:?}", msg),
                                    }
                                    buffer.truncate_front(buffer.len() - used);
                                }
                                Err(ref e) => {
                                    match *e {
                                        esercom::error::Error::EndNotFound => (),
                                        esercom::error::Error::InvalidLength(l) => {
                                            buffer.truncate_front(buffer.len() - l);
                                        }
                                        _ => {
                                            println!("Bad {:?}", e);
                                        }
                                    }
                                    break;
                                }
                            }
                        }
                    }
                    Err(ref e) if e.kind() == io::ErrorKind::TimedOut => (),
                    Err(e) => eprintln!("{:?}", e),
                }
            }
        }
        Err(e) => {
            eprintln!("Failed to open \"{}\". Error: {}", port_name, e);
            ::std::process::exit(1);
        }
    }
}
