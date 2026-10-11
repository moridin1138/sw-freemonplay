//! OpenH264 software decoding (H.264 only): the last resort when the OS has no decoder.

use anyhow::Result;
use openh264::formats::YUVSource;

use super::{Decoder, Deliver, Planes, YuvFrame};

pub struct SoftwareDecoder {
    decoder: openh264::decoder::Decoder,
    deliver: Deliver,
}

impl SoftwareDecoder {
    pub fn new(deliver: Deliver) -> Result<Self> {
        Ok(Self { decoder: openh264::decoder::Decoder::new()?, deliver })
    }
}

impl Decoder for SoftwareDecoder {
    fn name(&self) -> String {
        "OpenH264 (software, bundled)".into()
    }

    fn decode(&mut self, access_unit: &[u8]) -> Result<()> {
        let Some(yuv) = self.decoder.decode(access_unit)? else { return Ok(()) };
        let (w, h) = yuv.dimensions();
        let (sy, su, _) = yuv.strides();
        let ch = h.div_ceil(2);
        (self.deliver)(YuvFrame {
            width: w as u32,
            height: h as u32,
            planes: Planes::I420 {
                y: yuv.y()[..sy * h].to_vec(),
                u: yuv.u()[..su * ch].to_vec(),
                v: yuv.v()[..su * ch].to_vec(),
                y_stride: sy as u32,
                uv_stride: su as u32,
            },
        });
        Ok(())
    }
}
