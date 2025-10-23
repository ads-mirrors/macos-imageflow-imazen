use crate::errors::OutwardErrorBuffer;
use crate::ffi;
use crate::ffi::ImageflowJsonResponse;
use crate::flow::definitions::Graph;
use crate::for_other_imageflow_crates::preludes::external_without_std::*;
use crate::io::IoProxy;
use crate::{ErrorKind, FlowError, JsonResponse, Result};
use imageflow_types::collections::AddRemoveSet;
use std::any::Any;

use crate::allocation_container::AllocationContainer;
use crate::codecs::CodecInstanceContainer;
use crate::codecs::EnabledCodecs;
use crate::ffi::IoDirection;
use crate::graphics::bitmaps::{Bitmap, BitmapKey, BitmapWindowMut, BitmapsContainer};
use imageflow_types::ImageInfo;
use itertools::Itertools;

/// Something of a god object (which is necessary for a reasonable FFI interface).
pub struct Context {
    /// Buffer for errors presented to users of an FFI interface
    outward_error: OutwardErrorBuffer,
    pub debug_job_id: i32,
    pub next_stable_node_id: i32,
    pub next_graph_version: i32,
    pub max_calc_flatten_execute_passes: i32,
    pub graph_recording: s::Build001GraphRecording,

    /// Codecs, which in turn connect to I/O instances.
    pub codecs: AddRemoveSet<CodecInstanceContainer>, // This loans out exclusive mutable references to items, bounding the ownership lifetime to Context
    /// A list of io_ids already in use
    pub io_id_list: RefCell<Vec<i32>>,

    pub enabled_codecs: EnabledCodecs,

    pub security: imageflow_types::ExecutionSecurity,

    pub bitmaps: RefCell<crate::graphics::bitmaps::BitmapsContainer>,

    pub allocations: RefCell<AllocationContainer>,
}

//TODO: isn't this supposed to increment with each new context in process?
static mut JOB_ID: i32 = 0;

impl Context {
    pub fn create() -> Result<Box<Context>> {
        Context::create_cant_panic()
    }

    pub fn create_can_panic() -> Result<Box<Context>> {
        Ok(Box::new(Context {
            outward_error: OutwardErrorBuffer::new(),
            debug_job_id: unsafe { JOB_ID },
            next_graph_version: 0,
            next_stable_node_id: 0,
            max_calc_flatten_execute_passes: 40,
            graph_recording: s::Build001GraphRecording::off(),
            codecs: AddRemoveSet::with_capacity(4),
            io_id_list: RefCell::new(Vec::with_capacity(2)),
            enabled_codecs: EnabledCodecs::default(),
            bitmaps: RefCell::new(crate::graphics::bitmaps::BitmapsContainer::with_capacity(16)),
            security: imageflow_types::ExecutionSecurity {
                max_decode_size: None,
                max_frame_size: Some(imageflow_types::FrameSizeLimit {
                    w: 10000,
                    h: 10000,
                    megapixels: 100f32,
                }),
                max_encode_size: None,
            },
            allocations: RefCell::new(AllocationContainer::new()),
        }))
    }

    pub fn create_cant_panic() -> Result<Box<Context>> {
        std::panic::catch_unwind(|| {
            // Upgrade backtraces
            // Disable backtraces for debugging across the FFI boundary
            //imageflow_helpers::debug::upgrade_panic_hook_once_if_backtraces_wanted();

            Context::create_can_panic()
        })
        .unwrap_or_else(|_| Err(err_oom!())) //err_oom because it doesn't allocate anything.
    }

    /// Used by abi; should not panic
    pub fn abi_begin_terminate(&mut self) -> bool {
        self.codecs.mut_clear();
        true
    }
    pub fn destroy(mut self) -> Result<()> {
        self.abi_begin_terminate();
        Ok(())
    }

    pub fn outward_error(&self) -> &OutwardErrorBuffer {
        &self.outward_error
    }
    pub fn outward_error_mut(&mut self) -> &mut OutwardErrorBuffer {
        &mut self.outward_error
    }

    pub fn message(&mut self, method: &str, json: &[u8]) -> (JsonResponse, Result<()>) {
        crate::json::invoke_with_json_error_and_cancellation::<fn() -> bool>(self, method, json, None)
    }
    pub fn message_with_cancellation<F>(&mut self, method: &str, json: &[u8], poll_cancellation: Option<F>) -> (JsonResponse, Result<()>)  where F: Fn() -> bool + Send + Sync + 'static {
        crate::json::invoke_with_json_error_and_cancellation(self, method, json, poll_cancellation)
    }

    pub fn borrow_bitmaps_mut(&self) -> Result<RefMut<'_, BitmapsContainer>> {
        self.bitmaps.try_borrow_mut().map_err(|e| {
            nerror!(ErrorKind::FailedBorrow, "Failed to mutably borrow bitmaps collection: {:?}", e)
        })
    }
    pub fn borrow_bitmaps(&self) -> Result<Ref<'_, BitmapsContainer>> {
        self.bitmaps.try_borrow().map_err(|e| {
            nerror!(ErrorKind::FailedBorrow, "Failed to borrow bitmaps collection: {:?}", e)
        })
    }

    /// mem_calloc should not panic
    pub unsafe fn mem_calloc(
        &self,
        bytes: usize,
        alignment: usize,
        filename: *const libc::c_char,
        line: i32,
    ) -> Result<*mut u8> {
        let mut allocations = self.allocations.try_borrow_mut().map_err(|e| {
            let filename_str = if filename.is_null() {
                "[no filename provided]"
            } else {
                let c_filename = CStr::from_ptr(filename);
                c_filename.to_str().unwrap_or("[non UTF-8 filename]")
            };

            nerror!(
                ErrorKind::FailedBorrow,
                "Failed to mutably borrow allocations collection: {:?}\n{}:{}",
                e,
                filename_str,
                line
            )
        })?;

        let result = allocations.allocate(bytes, alignment).map_err(|e| {
            let filename_str = if filename.is_null() {
                "[no filename provided]"
            } else {
                let c_filename = CStr::from_ptr(filename);
                c_filename.to_str().unwrap_or("[non UTF-8 filename]")
            };

            nerror!(
                ErrorKind::AllocationFailed,
                "Failed to allocate {} bytes with alignment {}: {:?}\n{}:{}",
                bytes,
                alignment,
                e,
                filename_str,
                line
            )
        })?;
        Ok(result)
    }

    /// mem_calloc should not panic
    pub unsafe fn mem_free(&self, ptr: *const u8) -> bool {
        self.allocations.try_borrow_mut().map(|mut list| list.free(ptr)).unwrap_or(false)
    }

    pub fn io_id_present(&self, io_id: i32) -> bool {
        self.io_id_list.borrow().contains(&io_id)
    }

    fn add_io(&self, io: IoProxy, io_id: i32, direction: IoDirection) -> Result<()> {
        self.io_id_list.borrow_mut().push(io_id);

        let codec_value = CodecInstanceContainer::create(self, io, io_id, direction)
            .map_err(|e| e.at(here!()))?;
        let mut codec = self.codecs.add_mut(codec_value);
        if let Ok(d) = codec.get_decoder() {
            d.initialize(self).map_err(|e| e.at(here!()))?;
        }
        Ok(())
    }

    pub fn get_output_buffer_slice(&self, io_id: i32) -> Result<&[u8]> {
        let codec = self.get_codec(io_id).map_err(|e| e.at(here!()))?;
        let result = if let Some(io) = codec.get_encode_io().map_err(|e| e.at(here!()))? {
            io.map(|io| io.get_output_buffer_bytes(self).map_err(|e| e.at(here!())))
        } else {
            Err(nerror!(ErrorKind::InvalidArgument, "io_id {} is not an output buffer", io_id))
        };
        result
    }

    pub fn add_file(&mut self, io_id: i32, direction: IoDirection, path: &str) -> Result<()> {
        let io =
            IoProxy::file_with_mode(self, io_id, path, direction).map_err(|e| e.at(here!()))?;
        self.add_io(io, io_id, direction).map_err(|e| e.at(here!()))
    }

    pub fn add_copied_input_buffer(&mut self, io_id: i32, bytes: &[u8]) -> Result<()> {
        let io = IoProxy::copy_slice(self, io_id, bytes).map_err(|e| e.at(here!()))?;

        self.add_io(io, io_id, IoDirection::In).map_err(|e| e.at(here!()))
    }
    pub fn add_input_vector(&mut self, io_id: i32, bytes: Vec<u8>) -> Result<()> {
        let io = IoProxy::read_vec(self, io_id, bytes).map_err(|e| e.at(here!()))?;

        self.add_io(io, io_id, IoDirection::In).map_err(|e| e.at(here!()))
    }

    pub fn add_input_bytes<'b>(&'b mut self, io_id: i32, bytes: &'b [u8]) -> Result<()> {
        self.add_input_buffer(io_id, bytes)
    }
    pub fn add_input_buffer<'b>(&'b mut self, io_id: i32, bytes: &'b [u8]) -> Result<()> {
        let io = unsafe { IoProxy::read_slice(self, io_id, bytes) }.map_err(|e| e.at(here!()))?;

        self.add_io(io, io_id, IoDirection::In).map_err(|e| e.at(here!()))
    }

    pub fn add_output_buffer(&mut self, io_id: i32) -> Result<()> {
        let io = IoProxy::create_output_buffer(self, io_id).map_err(|e| e.at(here!()))?;

        self.add_io(io, io_id, IoDirection::Out).map_err(|e| e.at(here!()))
    }

    fn swap_dimensions_by_exif(&mut self, io_id: i32, image_info: &mut ImageInfo) -> Result<()> {
        let exif_maybe = self
            .get_codec(io_id)
            .map_err(|e| e.at(here!()))?
            .get_decoder()
            .map_err(|e| e.at(here!()))?
            .get_exif_rotation_flag(self)
            .map_err(|e| e.at(here!()))?;

        if let Some(exif_flag) = exif_maybe {
            if (5..=8).contains(&exif_flag) {
                std::mem::swap(&mut image_info.image_width, &mut image_info.image_height);
            }
        }
        Ok(())
    }

    pub fn get_unscaled_unrotated_image_info(&self, io_id: i32) -> Result<s::ImageInfo> {
        self.get_codec(io_id)
            .map_err(|e| e.at(here!()))?
            .get_decoder()
            .map_err(|e| e.at(here!()))?
            .get_unscaled_image_info(self)
            .map_err(|e| e.at(here!()))
    }

    pub fn get_unscaled_rotated_image_info(&mut self, io_id: i32) -> Result<s::ImageInfo> {
        let mut image_info =
            self.get_unscaled_unrotated_image_info(io_id).map_err(|e| e.at(here!()))?;

        self.swap_dimensions_by_exif(io_id, &mut image_info)?;
        Ok(image_info)
    }

    pub fn get_image_decodes(&mut self) -> Vec<s::DecodeResult> {
        let io_ids = self.io_id_list.borrow().to_vec();

        io_ids
            .iter()
            .filter_map(|io_id| {
                if let Ok(info) = self.get_unscaled_rotated_image_info(*io_id) {
                    Some(imageflow_types::DecodeResult {
                        io_id: *io_id,
                        preferred_extension: info.preferred_extension,
                        preferred_mime_type: info.preferred_mime_type,
                        w: info.image_width,
                        h: info.image_width,
                    })
                } else {
                    None
                }
            })
            .sorted_by_key(|r| r.io_id)
            .collect_vec()
    }

    pub fn get_scaled_unrotated_image_info(&mut self, io_id: i32) -> Result<s::ImageInfo> {
        self.get_codec(io_id)
            .map_err(|e| e.at(here!()))?
            .get_decoder()
            .map_err(|e| e.at(here!()))?
            .get_scaled_image_info(self)
            .map_err(|e| e.at(here!()))
    }

    pub fn get_scaled_rotated_image_info(&mut self, io_id: i32) -> Result<s::ImageInfo> {
        let mut image_info =
            self.get_scaled_unrotated_image_info(io_id).map_err(|e| e.at(here!()))?;

        self.swap_dimensions_by_exif(io_id, &mut image_info)?;
        Ok(image_info)
    }

    pub fn tell_decoder(&mut self, io_id: i32, tell: s::DecoderCommand) -> Result<()> {
        self.get_codec(io_id)
            .map_err(|e| e.at(here!()))?
            .get_decoder()
            .map_err(|e| e.at(here!()))?
            .tell_decoder(self, tell)
            .map_err(|e| e.at(here!()))
    }

    pub fn get_exif_rotation_flag(&mut self, io_id: i32) -> Result<Option<i32>> {
        self.get_codec(io_id)
            .map_err(|e| e.at(here!()))?
            .get_decoder()
            .map_err(|e| e.at(here!()))?
            .get_exif_rotation_flag(self)
            .map_err(|e| e.at(here!()))
    }

    pub fn get_codec(&self, io_id: i32) -> Result<RefMut<'_, CodecInstanceContainer>> {
        let mut borrow_errors = 0;
        for item_result in self.codecs.iter_mut() {
            if let Ok(container) = item_result {
                if container.io_id == io_id {
                    return Ok(container);
                }
            } else {
                borrow_errors += 1;
            }
        }
        if borrow_errors > 0 {
            Err(nerror!(ErrorKind::FailedBorrow, "Could not locate codec by io_id {}; some codecs were exclusively borrowed by another scope.", io_id))
        } else {
            Err(nerror!(
                ErrorKind::IoIdNotFound,
                "No codec with io_id {}; all codecs searched.",
                io_id
            ))
        }
    }

    pub fn build_1(&mut self, parsed: s::Build001) -> Result<s::ResponsePayload> {
        let job_result = self.build_inner_with_cancellation::<fn() -> bool>(parsed, None).map_err(|e| e.at(here!()))?;
        Ok(s::ResponsePayload::BuildResult(job_result))
    }

    /// For executing a complete job
    pub(crate) fn build_inner_with_cancellation<F>(&mut self, parsed: s::Build001, poll_cancellation: Option<F>) -> Result<s::JobResult> where F: Fn() -> bool + Send + Sync + 'static{
        let g = crate::parsing::GraphTranslator::new()
            .translate_framewise(parsed.framewise)
            .map_err(|e| e.at(here!()))?;

        if let Some(s::Build001Config { graph_recording, security, .. }) = parsed.builder_config {
            if let Some(r) = graph_recording {
                self.configure_graph_recording(r);
            }
            if let Some(s) = security {
                self.configure_security(s);
            }
        }

        crate::parsing::IoTranslator {}.add_all(self, parsed.io.clone())?;

        let decodes = self.get_image_decodes();

        let mut engine = crate::flow::execution_engine::Engine::create(self, g);

        let perf = engine.execute_many().map_err(|e| e.at(here!()))?;

        Ok(s::JobResult {
            decodes,
            encodes: engine.collect_augmented_encode_results(&parsed.io),
            performance: Some(perf),
        })
    }

    pub fn configure_graph_recording(&mut self, recording: s::Build001GraphRecording) {
        let r = if std::env::var("CI").map(|s| s.to_uppercase()) == Ok("TRUE".to_owned()) {
            s::Build001GraphRecording::off()
        } else {
            recording
        };
        self.graph_recording = r;
    }

    pub fn configure_security(&mut self, s: s::ExecutionSecurity) {
        if let Some(decode) = s.max_decode_size {
            self.security.max_decode_size = Some(decode);
        }
        if let Some(frame) = s.max_frame_size {
            self.security.max_frame_size = Some(frame);
        }
        if let Some(encode) = s.max_encode_size {
            self.security.max_encode_size = Some(encode);
        }
    }

    /// For executing an operation graph (assumes you have already configured the context with IO sources/destinations as needed)
    pub fn execute_1(&mut self, what: s::Execute001) -> Result<s::ResponsePayload> {
        let job_result = self.execute_inner(what).map_err(|e| e.at(here!()))?;
        Ok(s::ResponsePayload::JobResult(job_result))
    }
    pub(crate) fn execute_inner(&mut self, what: s::Execute001) -> Result<s::JobResult> {
        self.execute_inner_with_cancellation::<fn() -> bool>(what, None)
    }
    pub(crate) fn execute_inner_with_cancellation<F>(&mut self, what: s::Execute001, cancellation: Option<F>) -> Result<s::JobResult>
        where F: Fn() -> bool + Send + Sync + 'static
    {
        let g = crate::parsing::GraphTranslator::new()
            .translate_framewise(what.framewise)
            .map_err(|e| e.at(here!()))?;
        if let Some(r) = what.graph_recording {
            self.configure_graph_recording(r);
        }
        if let Some(s) = what.security {
            self.configure_security(s);
        }

        let decodes = self.get_image_decodes();

        let mut engine = crate::flow::execution_engine::Engine::create(self, g);

        let perf = engine.execute_many().map_err(|e| e.at(here!()))?;

        Ok(s::JobResult {
            decodes,
            encodes: engine.collect_encode_results(),
            performance: Some(perf),
        })
    }

    pub fn get_version_info(&self) -> Result<s::VersionInfo> {
        Context::get_version_info_static()
    }
    pub(crate) fn get_version_info_static() -> Result<s::VersionInfo> {
        Ok(s::VersionInfo {
            long_version_string: imageflow_types::version::one_line_version().to_string(),
            last_git_commit: imageflow_types::version::last_commit().to_string(),
            dirty_working_tree: imageflow_types::version::dirty(),
            build_date: imageflow_types::version::get_build_date().to_string(),
            git_tag: imageflow_types::version::get_build_env_value("GIT_OPTIONAL_TAG")
                .to_owned()
                .map(|s| s.to_string()),
            git_describe_always: imageflow_types::version::get_build_env_value(
                "GIT_DESCRIBE_ALWAYS",
            )
            .or(Some(""))
            .unwrap()
            .to_owned(),
        })
    }
}


/// cbindgen:ignore
#[derive(Debug, Clone, Copy)]
struct CancelCallbackData{ data: *mut libc::c_void }
/// cbindgen:ignore
unsafe impl  Send for CancelCallbackData {

}
/// cbindgen:ignore
unsafe impl Sync for CancelCallbackData {

}

#[cfg(test)]
fn test_get_output_buffer_slice_wrong_type_error() {
    let mut context = Context::create().unwrap();
    context.add_input_bytes(0, b"abcdef").unwrap();

    assert_eq!(ErrorKind::InvalidArgument, context.get_output_buffer_slice(0).err().unwrap().kind);
}

impl Drop for Context {
    /// Used by abi; should not panic
    fn drop(&mut self) {
        if let Err(e) = self.codecs.clear() {
            //TODO: log issue somewhere?
            eprintln!("Error clearing codecs in Context::drop: {:?}", e);
        }
        self.codecs.mut_clear(); // Dangerous, because there's no prohibition on dangling references.
    }
}

#[test]
fn test_context_size() {
    println!("std::mem::sizeof(Context) = {}", std::mem::size_of::<Context>());
    assert!(std::mem::size_of::<Context>() < 500);
}
