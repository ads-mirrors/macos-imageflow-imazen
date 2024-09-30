use std::io::Read;

use crate::{Context, Result, FlowError, ErrorKind};
use crate::io::IoProxy;
use crate::graphics::bitmaps::{BitmapKey, ColorSpace, BitmapCompositing};
use crate::codecs::Decoder;
use imageflow_types as s;
use imageflow_helpers::preludes::from_std::*;
//use crate::for_other_imageflow_crates::preludes::external_without_std::*;
use crate::graphics::bitmaps::BitmapRowAccess;

use png;
use rgb;

pub struct ImagePngDecoder {
    reader: png::Reader<IoProxy>,
    info: png::Info<'static>,
}

impl ImagePngDecoder {
    pub fn create(c: &Context, io: IoProxy, io_id: i32) -> Result<ImagePngDecoder> {
        // Wrap IoProxy in a Box<dyn Read>
        //let read_io = Box::new(io) as Box<dyn Read>;
        let mut decoder = png::Decoder::new(io);
        //decoder.set_transformations(png::Transformations::EXPAND);
        decoder.set_transformations(png::Transformations::normalize_to_color8());

        let reader = decoder.read_info()
            .map_err(|e| FlowError::from_png_decoder(e).at(here!()))?;

        let info = reader.info().clone();

        Ok(ImagePngDecoder {
            reader,
            info,
        })
    }
}
impl Decoder for ImagePngDecoder {
    fn initialize(&mut self, c: &Context) -> Result<()> {
        Ok(())
    }

    fn get_unscaled_image_info(&mut self, c: &Context) -> Result<s::ImageInfo> {
        Ok(s::ImageInfo {
            frame_decodes_into: s::PixelFormat::Bgra32,
            image_width: self.info.width as i32,
            image_height: self.info.height as i32,
            preferred_mime_type: "image/png".to_owned(),
            preferred_extension: "png".to_owned(),
        })
    }

    fn get_scaled_image_info(&mut self, c: &Context) -> Result<s::ImageInfo> {
        self.get_unscaled_image_info(c)
    }

    fn get_exif_rotation_flag(&mut self, c: &Context) -> Result<Option<i32>> {
        Ok(None)
    }

    fn tell_decoder(&mut self, c: &Context, tell: s::DecoderCommand) -> Result<()> {
        Ok(())
    }

    fn read_frame(&mut self, c: &Context) -> Result<BitmapKey> {
        let mut bitmaps = c.borrow_bitmaps_mut().map_err(|e| e.at(here!()))?;
        let info = self.reader.info();

        let canvas_key = bitmaps.create_bitmap_u8(
            info.width,
            info.height,
            imageflow_types::PixelLayout::BGRA,
            false,
            true,
            ColorSpace::StandardRGB,
            BitmapCompositing::ReplaceSelf,
        ).map_err(|e| e.at(here!()))?;

        let mut bitmap =
            bitmaps.try_borrow_mut(canvas_key)
                .map_err(|e| e.at(here!()))?;

        let mut canvas = bitmap.get_window_u8().unwrap();


        let mut buffer = vec![0; self.reader.output_buffer_size()];
        let output_info = self.reader.next_frame(&mut buffer).map_err(|e| FlowError::from_png_decoder(e).at(here!()))?;

        let h = output_info.height as usize;
        let stride = output_info.line_size;
        let w = output_info.width as usize;
        if output_info.bit_depth != png::BitDepth::Eight{
            return Err(nerror!(ErrorKind::ImageDecodingError, "image/png decoder did not expand to 8-bit channels").at(here!()));
        }
        match output_info.color_type{
            png::ColorType::Rgb =>{
                for row_ix in 0..h{
                    let from = buffer.row_mut_rgb8(row_ix, stride).unwrap();
                    let to = canvas.row_mut_bgra(row_ix as u32).unwrap();
                    from.into_iter().zip(to.iter_mut()).for_each(|(from, to)| {
                        to.r = from.r;
                        to.g = from.g;
                        to.b = from.b;
                        to.a = 255;
                    });
                }
            },
            png::ColorType::Rgba => {
                for row_ix in 0..h{
                    let from = buffer.row_mut_rgba8(row_ix, stride).unwrap();
                    let to = canvas.row_mut_bgra(row_ix as u32).unwrap();
                    from.into_iter().zip(to.iter_mut()).for_each(|(from, to)| {
                        to.r = from.r;
                        to.g = from.g;
                        to.b = from.b;
                        to.a = from.a;
                    });
                }
            },

            png::ColorType::Grayscale => {
                for row_ix in 0..h{
                    let from = buffer.row_mut_gray8(row_ix, stride).unwrap();
                    let to = canvas.row_mut_bgra(row_ix as u32).unwrap();
                    from.into_iter().zip(to.iter_mut()).for_each(|(f, to)| {
                        to.r = f.v;
                        to.g = f.v;
                        to.b = f.v;
                        to.a = 255;
                    });
                }

            },
            png::ColorType::GrayscaleAlpha => {
                for row_ix in 0..h{
                    let from = buffer.row_mut_grayalpha8(row_ix, stride).unwrap();
                    let to = canvas.row_mut_bgra(row_ix as u32).unwrap();
                        from.into_iter().zip(to.iter_mut()).for_each(|(from, to)| {
                        to.r = from.v;
                        to.g = from.v;
                        to.b = from.v;
                        to.a = from.a;
                    });
                }
            }

            _ => panic!("png decoder bug: indexed image was not expanded despite flags.")
        }

        Ok(canvas_key)
    }

    fn has_more_frames(&mut self) -> Result<bool> {
        Ok(false)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self as &dyn std::any::Any
    }
}
