use anyhow::{Result, bail, ensure};
use std::{collections::VecDeque, time::Duration};
use web_time::Instant;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub command: u8,
    pub payload: Vec<u8>,
}
impl Frame {
    pub fn encoded(&self) -> Result<Vec<u8>> {
        ensure!(
            self.payload.len() <= 255,
            "Niimbot packet payload is too large"
        );
        let length = self.payload.len() as u8;
        let checksum = self
            .payload
            .iter()
            .fold(self.command ^ length, |a, b| a ^ b);
        let mut bytes = vec![0x55, 0x55, self.command, length];
        bytes.extend(&self.payload);
        bytes.extend([checksum, 0xaa, 0xaa]);
        Ok(bytes)
    }
}
#[derive(Default)]
pub struct Decoder {
    buffer: Vec<u8>,
}
impl Decoder {
    pub fn append(&mut self, bytes: &[u8]) -> Vec<Frame> {
        self.buffer.extend(bytes);
        let mut result = Vec::new();
        while self.buffer.len() >= 2 {
            if self.buffer[..2] != [0x55, 0x55] {
                self.buffer.remove(0);
                continue;
            }
            if self.buffer.len() < 4 {
                break;
            }
            let total = self.buffer[3] as usize + 7;
            if self.buffer.len() < total {
                if let Some(next) = (1..self.buffer.len().saturating_sub(6))
                    .find(|&i| valid_frame(&self.buffer[i..]))
                {
                    self.buffer.drain(..next);
                    continue;
                }
                break;
            }
            if !valid_frame(&self.buffer) {
                self.buffer.remove(0);
                continue;
            }
            result.push(Frame {
                command: self.buffer[2],
                payload: self.buffer[4..total - 3].to_vec(),
            });
            self.buffer.drain(..total);
        }
        result
    }
}
fn valid_frame(bytes: &[u8]) -> bool {
    if bytes.len() < 7 || bytes[..2] != [0x55, 0x55] {
        return false;
    }
    let total = bytes[3] as usize + 7;
    bytes.len() >= total
        && bytes[total - 2..total] == [0xaa, 0xaa]
        && bytes[total - 3]
            == bytes[4..total - 3]
                .iter()
                .fold(bytes[2] ^ bytes[3], |a, b| a ^ b)
}
pub struct Raster {
    pub width: u16,
    pub height: u16,
    pub bytes: Vec<u8>,
}
impl Raster {
    pub fn packed_rgba(width: u16, height: u16, rgba: &[u8], offset_y: usize) -> Result<Self> {
        ensure!(
            width > 0
                && width <= 384
                && height > 0
                && rgba.len() == width as usize * height as usize * 4,
            "Invalid printer bitmap dimensions"
        );
        let stride = (width as usize).div_ceil(8);
        let mut bytes = vec![0; stride * height as usize];
        for y in offset_y..height as usize {
            for x in 0..width as usize {
                let base = ((y - offset_y) * width as usize + x) * 4;
                let p = &rgba[base..base + 4];
                if p[3] > 32
                    && 299 * u32::from(p[0]) + 587 * u32::from(p[1]) + 114 * u32::from(p[2])
                        < 128_000
                {
                    bytes[y * stride + x / 8] |= 0x80 >> (x % 8);
                }
            }
        }
        Ok(Self {
            width,
            height,
            bytes,
        })
    }
    pub fn rows(&self) -> Result<Vec<Frame>> {
        let stride = (self.width as usize).div_ceil(8);
        let height = self.height as usize;
        ensure!(
            self.width > 0
                && self.width <= 384
                && height > 0
                && self.bytes.len() == stride * height,
            "Invalid B1 label raster"
        );
        let mut frames = Vec::new();
        let mut row = 0;
        while row < height {
            let pixels = &self.bytes[row * stride..(row + 1) * stride];
            let mut run = 1;
            while row + run < height
                && run < 200
                && &self.bytes[(row + run) * stride..(row + run + 1) * stride] == pixels
            {
                run += 1;
            }
            let mut payload = vec![(row >> 8) as u8, row as u8];
            let command = if pixels.iter().all(|b| *b == 0) {
                payload.push(run as u8);
                0x84
            } else {
                let count: u16 = pixels.iter().map(|b| b.count_ones() as u16).sum();
                payload.extend([0, count as u8, (count >> 8) as u8, run as u8]);
                payload.extend(pixels);
                0x85
            };
            frames.push(Frame { command, payload });
            row += run;
        }
        Ok(frames)
    }
    pub fn page_size(copies: u8) -> Result<Vec<u8>> {
        ensure!((1..=20).contains(&copies), "Choose 1–20 copies");
        Ok(vec![0, 240, 1, 128, 0, copies])
    }
}
/// BLE framing is platform independent. Reads and writes must have bounded waits.
#[allow(async_fn_in_trait)]
pub trait Transport {
    fn now(&self) -> Instant {
        Instant::now()
    }
    async fn write(&mut self, bytes: &[u8]) -> Result<()>;
    fn try_read(&mut self) -> Result<Option<Vec<u8>>>;
    async fn read(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>>;
    async fn pause(&mut self, duration: Duration);
    async fn connected(&mut self) -> bool;
    async fn disconnect(&mut self) -> Result<()>;
}
pub struct Printer<T: Transport> {
    pub transport: T,
    decoder: Decoder,
    replies: VecDeque<Frame>,
    highest_row: Option<u16>,
}
impl<T: Transport> Printer<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            decoder: Decoder::default(),
            replies: VecDeque::new(),
            highest_row: None,
        }
    }
    fn received(&mut self, bytes: &[u8]) {
        for frame in self.decoder.append(bytes) {
            if frame.command == 0xd3 && frame.payload.len() >= 2 {
                let row = u16::from_be_bytes([frame.payload[0], frame.payload[1]]);
                self.highest_row = Some(self.highest_row.unwrap_or(0).max(row));
            }
            self.replies.push_back(frame);
            if self.replies.len() > 256 {
                self.replies.pop_front();
            }
        }
    }
    async fn command(
        &mut self,
        code: u8,
        payload: &[u8],
        response: Option<u8>,
        timeout: Duration,
    ) -> Result<Frame> {
        // Drain notifications received before this command, including row progress.
        while let Some(bytes) = self.transport.try_read()? {
            self.received(&bytes);
        }
        self.replies.clear();
        self.transport
            .write(
                &Frame {
                    command: code,
                    payload: payload.to_vec(),
                }
                .encoded()?,
            )
            .await?;
        let deadline = self.transport.now() + timeout;
        loop {
            if let Some(index) = self
                .replies
                .iter()
                .position(|f| response.map_or(f.command != 0xd3, |r| f.command == r))
            {
                return Ok(self.replies.remove(index).unwrap());
            }
            let remaining = deadline.saturating_duration_since(self.transport.now());
            if remaining.is_zero() {
                break;
            }
            if let Some(bytes) = self.transport.read(remaining).await? {
                self.received(&bytes);
            } else {
                break;
            }
        }
        bail!(
            "No printer acknowledgement for command 0x{code:02X}. Check paper, lid, and Bluetooth connection."
        )
    }
    async fn ack(&mut self, code: u8, payload: &[u8], response: u8) -> Result<Frame> {
        self.command(code, payload, Some(response), Duration::from_secs(2))
            .await
    }
    pub async fn identify(&mut self) -> Result<()> {
        let _ = self
            .command(0xa5, &[1], Some(0xb5), Duration::from_secs(1))
            .await;
        let model = self.ack(0x40, &[8], 0x48).await?;
        ensure!(
            model.payload.len() >= 2
                && u16::from_be_bytes([model.payload[0], model.payload[1]]) == 4096,
            "This print path supports Niimbot B1 (203 dpi), not B1 Pro or other models"
        );
        let _ = self
            .command(0xa5, &[1], Some(0xb5), Duration::from_secs(1))
            .await;
        for sub in [8, 11, 13, 10, 7, 3, 12, 9] {
            let _ = self
                .command(0x40, &[sub], None, Duration::from_millis(600))
                .await;
        }
        let _ = self
            .command(0xdc, &[4], Some(0xd9), Duration::from_secs(1))
            .await;
        Ok(())
    }
    /// Exactly one PrintEnd attempt after a started job. Never automatically retries a job.
    pub async fn print(
        &mut self,
        raster: &Raster,
        copies: u8,
        density: u8,
        mut progress: impl FnMut(String),
    ) -> Result<()> {
        ensure!((1..=5).contains(&density), "Choose density 1–5");
        ensure!(
            raster.width == 384 && raster.height == 240,
            "B1 requires a 384 × 240 label"
        );
        let page = Raster::page_size(copies)?;
        let frames = raster.rows()?;
        let mut started = false;
        let result: Result<()> = async {
            progress("Configuring label…".into());
            self.ack(0x21, &[density], 0x31).await?;
            self.ack(0x23, &[1], 0x33).await?;
            started = true;
            self.ack(0x01, &[0, copies, 0, 0, 0, 0, 0], 0x02).await?;
            self.ack(0x03, &[1], 0x04).await?;
            self.ack(0x13, &page, 0x14).await?;
            self.highest_row = None;
            for (index, frame) in frames.iter().enumerate() {
                progress(format!("Sending label • {}%", (index + 1) * 100 / frames.len()));
                self.transport.write(&frame.encoded()?).await?;
            }
            self.command(0xe3, &[1], Some(0xe4), Duration::from_secs(10)).await?;
            if let Some(row) = self.highest_row {
                ensure!(row >= raster.height - 1,
                    "Printer only confirmed image row {} of {}. The label may be incomplete.",
                    row + 1, raster.height);
            }
            progress("Waiting for print confirmation…".into());
            let deadline = self.transport.now() + Duration::from_secs(25);
            let mut confirmed = false;
            while self.transport.now() < deadline {
                if let Ok(reply) = self.command(0xa3, &[1], Some(0xb3), Duration::from_millis(900)).await
                    && reply.payload.len() >= 4
                {
                    let pages = u16::from_be_bytes([reply.payload[0], reply.payload[1]]);
                    progress(format!("Printed {pages} of {copies} labels"));
                    if pages >= u16::from(copies) { confirmed = true; break; }
                }
                ensure!(self.transport.connected().await, "Printer disconnected before print confirmation.");
                self.transport.pause(Duration::from_millis(150)).await;
            }
            // Mark before writing: a failed PrintEnd must not cause a second attempt.
            started = false;
            self.command(0xf3, &[1], Some(0xf4), Duration::from_millis(2500)).await?;
            ensure!(confirmed, "Print was not confirmed. Check the label before retrying; it may already have printed.");
            Ok(())
        }.await;
        if result.is_err() && started {
            let _ = self
                .command(0xf3, &[1], Some(0xf4), Duration::from_millis(2500))
                .await;
        }
        result.map_err(|e| anyhow::anyhow!("{e} Check the label before retrying."))?;
        progress(format!(
            "Printed {copies} label{} • confirmed by B1",
            if copies == 1 { "" } else { "s" }
        ));
        Ok(())
    }
}
