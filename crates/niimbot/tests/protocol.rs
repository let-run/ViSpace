use anyhow::{Result, bail};
use inventory_niimbot::*;
use std::{collections::VecDeque, time::Duration};
use web_time::Instant;
fn frame(command: u8, payload: &[u8]) -> Frame {
    Frame {
        command,
        payload: payload.to_vec(),
    }
}
#[test]
fn swift_framing_and_stream_recovery() {
    let f = frame(0x21, &[3]);
    let encoded = f.encoded().unwrap();
    assert_eq!(encoded, [0x55, 0x55, 0x21, 1, 3, 0x23, 0xaa, 0xaa]);
    let mut decoder = Decoder::default();
    assert!(
        decoder
            .append(&[&[0, 0x55], &encoded[..3]].concat())
            .is_empty()
    );
    assert_eq!(
        decoder.append(&[&encoded[3..], &encoded].concat()),
        [f.clone(), f.clone()]
    );
    let mut corrupt = encoded.clone();
    corrupt[5] ^= 1;
    assert_eq!(
        decoder
            .append(&[corrupt, encoded.clone()].concat())
            .as_slice(),
        std::slice::from_ref(&f)
    );
    assert_eq!(
        decoder.append(&[vec![0x55, 0x55, 0x99, 255], encoded].concat()),
        [f]
    );
    assert!(frame(0, &[0; 256]).encoded().is_err());
}
#[test]
fn swift_row_packets_and_limits() {
    let raster = Raster {
        width: 16,
        height: 3,
        bytes: vec![0, 0, 0x80, 1, 0x80, 1],
    };
    assert_eq!(
        raster.rows().unwrap(),
        [
            frame(0x84, &[0, 0, 1]),
            frame(0x85, &[0, 1, 0, 2, 0, 2, 0x80, 1])
        ]
    );
    let blank = Raster {
        width: 8,
        height: 401,
        bytes: vec![0; 401],
    };
    assert_eq!(
        blank
            .rows()
            .unwrap()
            .iter()
            .map(|f| f.payload.clone())
            .collect::<Vec<_>>(),
        [vec![0, 0, 200], vec![0, 200, 200], vec![1, 144, 1]]
    );
    let black = Raster {
        width: 384,
        height: 1,
        bytes: vec![255; 48],
    };
    assert_eq!(
        &black.rows().unwrap()[0].payload[..6],
        &[0, 0, 0, 128, 1, 1]
    );
    assert!(
        Raster {
            width: 384,
            height: 240,
            bytes: vec![]
        }
        .rows()
        .is_err()
    );
    assert_eq!(Raster::page_size(2).unwrap(), [0, 240, 1, 128, 0, 2]);
    assert!(Raster::page_size(0).is_err());
    assert!(Raster::page_size(21).is_err());
}
#[test]
fn rgba_threshold_padding_alpha_and_paper_offset() {
    let mut rgba = vec![255; 9 * 2 * 4];
    for x in [0, 8] {
        rgba[x * 4..x * 4 + 3].fill(0);
    }
    assert_eq!(
        Raster::packed_rgba(9, 2, &rgba, 0).unwrap().bytes,
        [0x80, 0x80, 0, 0]
    );
    assert_eq!(
        Raster::packed_rgba(9, 2, &rgba, 1).unwrap().bytes,
        [0, 0, 0x80, 0x80]
    );
    rgba[3] = 0;
    assert_eq!(
        Raster::packed_rgba(9, 2, &rgba, 0).unwrap().bytes,
        [0, 0x80, 0, 0]
    );
}
struct Fake {
    commands: Vec<Frame>,
    pending: VecDeque<Vec<u8>>,
    clock: Instant,
    elapsed: Duration,
    model: u16,
    fail: Option<u8>,
    confirmed: bool,
    highest: Option<u16>,
    missing_ack: Option<u8>,
}
impl Default for Fake {
    fn default() -> Self {
        Self {
            commands: vec![],
            pending: VecDeque::new(),
            clock: Instant::now(),
            elapsed: Duration::ZERO,
            model: 4096,
            fail: None,
            confirmed: true,
            highest: Some(239),
            missing_ack: None,
        }
    }
}
impl Transport for Fake {
    fn now(&self) -> Instant {
        self.clock + self.elapsed
    }
    async fn write(&mut self, bytes: &[u8]) -> Result<()> {
        // Exercise byte-stream decoding even though the fake transport receives a complete write.
        let mut decoder = Decoder::default();
        let mut frames = Vec::new();
        for chunk in bytes.chunks(3) {
            frames.extend(decoder.append(chunk));
        }
        for f in frames {
            self.commands.push(f.clone());
            if self.fail == Some(f.command) {
                bail!("Simulated upload interruption");
            }
            if self.missing_ack == Some(f.command) {
                continue;
            }
            let reply = match f.command {
                0xa5 => Some(frame(0xb5, &[1])),
                0x40 => Some(frame(0x48, &self.model.to_be_bytes())),
                0xdc => Some(frame(0xd9, &[1])),
                0x21 => Some(frame(0x31, &[1])),
                0x23 => Some(frame(0x33, &[1])),
                0x01 => Some(frame(0x02, &[1])),
                0x03 => Some(frame(0x04, &[1])),
                0x13 => Some(frame(0x14, &[1])),
                0xe3 => {
                    if let Some(row) = self.highest {
                        self.pending
                            .push_back(frame(0xd3, &row.to_be_bytes()).encoded()?);
                    }
                    Some(frame(0xe4, &[1]))
                }
                0xa3 => Some(frame(0xb3, &[0, if self.confirmed { 20 } else { 0 }, 0, 0])),
                0xf3 => Some(frame(0xf4, &[1])),
                _ => None,
            };
            if let Some(reply) = reply {
                self.pending.push_back(reply.encoded()?);
            }
        }
        Ok(())
    }
    fn try_read(&mut self) -> Result<Option<Vec<u8>>> {
        Ok(self.pending.pop_front())
    }
    async fn read(&mut self, wait: Duration) -> Result<Option<Vec<u8>>> {
        if let Some(bytes) = self.pending.pop_front() {
            Ok(Some(bytes))
        } else {
            self.elapsed += wait;
            Ok(None)
        }
    }
    async fn pause(&mut self, d: Duration) {
        self.elapsed += d;
    }
    async fn connected(&mut self) -> bool {
        true
    }
    async fn disconnect(&mut self) -> Result<()> {
        Ok(())
    }
}
fn raster() -> Raster {
    let mut bytes = vec![0; 48 * 240];
    bytes[..48].fill(255);
    Raster {
        width: 384,
        height: 240,
        bytes,
    }
}
#[tokio::test]
async fn b1_handshake_and_confirmed_print_match_swift_sequence() {
    let mut printer = Printer::new(Fake::default());
    printer.identify().await.unwrap();
    assert_eq!(
        &printer.transport.commands[..3],
        &[frame(0xa5, &[1]), frame(0x40, &[8]), frame(0xa5, &[1])]
    );
    printer.transport.commands.clear();
    let mut progress = vec![];
    printer
        .print(&raster(), 2, 3, |m| progress.push(m))
        .await
        .unwrap();
    let frames = &printer.transport.commands;
    assert_eq!(
        &frames[..5],
        &[
            frame(0x21, &[3]),
            frame(0x23, &[1]),
            frame(1, &[0, 2, 0, 0, 0, 0, 0]),
            frame(3, &[1]),
            frame(0x13, &[0, 240, 1, 128, 0, 2])
        ]
    );
    assert_eq!(frames.last().unwrap(), &frame(0xf3, &[1]));
    assert_eq!(frames.iter().filter(|f| f.command == 0xf3).count(), 1);
    assert!(progress.last().unwrap().contains("confirmed by B1"));
}
#[tokio::test]
async fn wrong_model_and_invalid_options_never_start_a_print() {
    let mut printer = Printer::new(Fake {
        model: 4097,
        ..Default::default()
    });
    assert!(
        printer
            .identify()
            .await
            .unwrap_err()
            .to_string()
            .contains("not B1 Pro")
    );
    printer.transport.commands.clear();
    assert!(printer.print(&raster(), 0, 3, |_| {}).await.is_err());
    assert!(printer.print(&raster(), 1, 6, |_| {}).await.is_err());
    assert!(printer.transport.commands.is_empty());
}
#[tokio::test]
async fn interrupted_upload_attempts_end_exactly_once_without_retrying_job() {
    let mut printer = Printer::new(Fake {
        fail: Some(0x85),
        ..Default::default()
    });
    assert!(
        printer
            .print(&raster(), 1, 3, |_| {})
            .await
            .unwrap_err()
            .to_string()
            .contains("Check the label before retrying")
    );
    assert_eq!(
        printer
            .transport
            .commands
            .iter()
            .filter(|f| f.command == 0xf3)
            .count(),
        1
    );
    assert_eq!(
        printer
            .transport
            .commands
            .iter()
            .filter(|f| f.command == 1)
            .count(),
        1
    );
}
#[tokio::test]
async fn incomplete_upload_and_unconfirmed_print_are_errors() {
    for fake in [
        Fake {
            highest: Some(100),
            ..Default::default()
        },
        Fake {
            confirmed: false,
            ..Default::default()
        },
    ] {
        let mut printer = Printer::new(fake);
        let mut progress = vec![];
        assert!(
            printer
                .print(&raster(), 2, 3, |m| progress.push(m))
                .await
                .is_err()
        );
        assert_eq!(
            printer
                .transport
                .commands
                .iter()
                .filter(|f| f.command == 0xf3)
                .count(),
            1
        );
        assert!(!progress.iter().any(|m| m.contains("confirmed by B1")));
    }
}
#[tokio::test]
async fn stale_ack_does_not_satisfy_new_command() {
    let mut fake = Fake {
        missing_ack: Some(0x21),
        ..Default::default()
    };
    fake.pending.push_back(frame(0x31, &[1]).encoded().unwrap());
    let mut printer = Printer::new(fake);
    assert!(printer.print(&raster(), 1, 3, |_| {}).await.is_err());
    assert_eq!(printer.transport.commands.len(), 1);
    assert_eq!(printer.transport.commands[0].command, 0x21);
}
