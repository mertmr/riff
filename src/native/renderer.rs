use egui::epaint::{self, ClippedPrimitive, Primitive, TextureId, Vertex, textures::TexturesDelta};
use objc2::{
    rc::{Retained, autoreleasepool},
    runtime::ProtocolObject,
};
use objc2_foundation::NSString;
use objc2_metal::*;
use objc2_quartz_core::{CAMetalDrawable, CAMetalLayer};
use std::{collections::HashMap, ffi::c_void, ptr::NonNull};

pub struct Painter {
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    pipeline: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    sampler: Retained<ProtocolObject<dyn MTLSamplerState>>,
    textures: HashMap<TextureId, Retained<ProtocolObject<dyn MTLTexture>>>,
    vertices: Vec<Vertex>,
    indices: Vec<u32>,
    vertex_buffer: Option<Retained<ProtocolObject<dyn MTLBuffer>>>,
    index_buffer: Option<Retained<ProtocolObject<dyn MTLBuffer>>>,
    pending: Option<Retained<ProtocolObject<dyn MTLCommandBuffer>>>,
}
// The shader reads egui's packed vertex layout directly.
const _: () = assert!(std::mem::size_of::<Vertex>() == 20);
const _: () = assert!(std::mem::offset_of!(Vertex, pos) == 0);
const _: () = assert!(std::mem::offset_of!(Vertex, uv) == 8);
const _: () = assert!(std::mem::offset_of!(Vertex, color) == 16);
impl Painter {
    pub fn new(layer: &CAMetalLayer) -> Result<Self, String> {
        let device = layer.device().ok_or("Metal layer has no device")?;
        let queue = device
            .newCommandQueueWithMaxCommandBufferCount(2)
            .ok_or("Could not create Metal command queue")?;
        let shader = NSString::from_str(
            r#"
#include <metal_stdlib>
using namespace metal;
struct Vertex { packed_float2 pos; packed_float2 uv; uint color; };
struct Output { float4 position [[position]]; float2 uv; float4 color; };
vertex Output riff_vertex(uint id [[vertex_id]], device const Vertex* vertices [[buffer(0)]], constant float2& screen [[buffer(1)]]) {
    Vertex v=vertices[id]; Output o;
    o.position=float4(2.*float2(v.pos).x/screen.x-1.,1.-2.*float2(v.pos).y/screen.y,0.,1.);
    o.uv=v.uv; o.color=float4(v.color&255,(v.color>>8)&255,(v.color>>16)&255,(v.color>>24)&255)/255.; return o;
}
fragment float4 riff_fragment(Output in [[stage_in]], texture2d<float> image [[texture(0)]], sampler sample [[sampler(0)]]) {
    return in.color * image.sample(sample,in.uv);
}
"#,
        );
        let library = device
            .newLibraryWithSource_options_error(&shader, None)
            .map_err(|e| format!("Metal shader: {e}"))?;
        let descriptor = MTLRenderPipelineDescriptor::new();
        descriptor.setVertexFunction(
            library
                .newFunctionWithName(&NSString::from_str("riff_vertex"))
                .as_deref(),
        );
        descriptor.setFragmentFunction(
            library
                .newFunctionWithName(&NSString::from_str("riff_fragment"))
                .as_deref(),
        );
        let color = unsafe { descriptor.colorAttachments().objectAtIndexedSubscript(0) };
        color.setPixelFormat(layer.pixelFormat());
        color.setBlendingEnabled(true);
        color.setSourceRGBBlendFactor(MTLBlendFactor::One);
        color.setDestinationRGBBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
        color.setSourceAlphaBlendFactor(MTLBlendFactor::One);
        color.setDestinationAlphaBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
        let pipeline = device
            .newRenderPipelineStateWithDescriptor_error(&descriptor)
            .map_err(|e| format!("Metal pipeline: {e}"))?;
        let descriptor = MTLSamplerDescriptor::new();
        descriptor.setMinFilter(MTLSamplerMinMagFilter::Linear);
        descriptor.setMagFilter(MTLSamplerMinMagFilter::Linear);
        descriptor.setSAddressMode(MTLSamplerAddressMode::ClampToEdge);
        descriptor.setTAddressMode(MTLSamplerAddressMode::ClampToEdge);
        let sampler = device
            .newSamplerStateWithDescriptor(&descriptor)
            .ok_or("Could not create Metal sampler")?;
        Ok(Self {
            device,
            queue,
            pipeline,
            sampler,
            textures: HashMap::new(),
            vertices: Vec::new(),
            indices: Vec::new(),
            vertex_buffer: None,
            index_buffer: None,
            pending: None,
        })
    }
    fn texture_descriptor(
        &self,
        format: MTLPixelFormat,
        size: [usize; 2],
    ) -> Retained<MTLTextureDescriptor> {
        // SAFETY: both upload paths validate nonempty images; their dimensions
        // come from egui's bounded atlas or the bounded artwork decoder.
        let descriptor = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                format, size[0], size[1], false,
            )
        };
        descriptor.setStorageMode(if self.device.hasUnifiedMemory() {
            MTLStorageMode::Shared
        } else {
            MTLStorageMode::Managed
        });
        descriptor.setUsage(MTLTextureUsage::ShaderRead);
        descriptor
    }
    fn buffer(
        device: &ProtocolObject<dyn MTLDevice>,
        buffer: &mut Option<Retained<ProtocolObject<dyn MTLBuffer>>>,
        size: usize,
    ) {
        if buffer.as_ref().is_none_or(|b| b.length() < size) {
            *buffer = device.newBufferWithLength_options(
                size.max(1024).next_power_of_two(),
                MTLResourceOptions::StorageModeShared,
            );
        }
    }
    pub fn paint(
        &mut self,
        layer: &CAMetalLayer,
        ppp: f32,
        primitives: &[ClippedPrimitive],
        delta: &mut TexturesDelta,
        capture: bool,
    ) -> Result<Option<egui::ColorImage>, String> {
        layer.setFramebufferOnly(!capture);
        let result = self.paint_frame(layer, ppp, primitives, delta, capture);
        layer.setFramebufferOnly(true);
        result
    }
    fn paint_frame(
        &mut self,
        layer: &CAMetalLayer,
        ppp: f32,
        primitives: &[ClippedPrimitive],
        delta: &mut TexturesDelta,
        capture: bool,
    ) -> Result<Option<egui::ColorImage>, String> {
        // SAFETY: all pointer uploads use checked texture regions and buffers sized
        // to the source slices. CPU writes/readback wait for preceding GPU work.
        // The command and its resources remain alive until completion; this renderer
        // is used only by the main-thread native runner.
        autoreleasepool(|_| unsafe {
            if let Some(pending) = self.pending.take() {
                pending.waitUntilCompleted();
                if pending.status() == MTLCommandBufferStatus::Error {
                    return Err(format!("Metal rendering failed: {:?}", pending.error()));
                }
            }
            for (id, updates) in delta.set.drain() {
                for update in updates {
                    let epaint::ImageData::Color(image) = update.image;
                    let size = image.size;
                    if size[0].checked_mul(size[1]) != Some(image.pixels.len())
                        || image.pixels.is_empty()
                    {
                        return Err("Invalid texture pixels".into());
                    }
                    let monochrome = image.pixels.iter().all(|p| p.to_array() == [p.a(); 4]);
                    if update.pos.is_none() {
                        // egui's dark-mode font atlas contains white coverage values.
                        // An R8 texture swizzled into all four channels gives identical
                        // sampling with a quarter of the GPU storage.
                        let coverage = id == TextureId::Managed(0) && monochrome;
                        let format = if coverage {
                            MTLPixelFormat::R8Unorm
                        } else {
                            MTLPixelFormat::RGBA8Unorm
                        };
                        let descriptor = self.texture_descriptor(format, size);
                        if coverage {
                            descriptor.setSwizzle(MTLTextureSwizzleChannels {
                                red: MTLTextureSwizzle::Red,
                                green: MTLTextureSwizzle::Red,
                                blue: MTLTextureSwizzle::Red,
                                alpha: MTLTextureSwizzle::Red,
                            });
                        }
                        self.textures.insert(
                            id,
                            self.device
                                .newTextureWithDescriptor(&descriptor)
                                .ok_or("Could not allocate Metal texture")?,
                        );
                    } else if !monochrome
                        && self
                            .textures
                            .get(&id)
                            .is_some_and(|t| t.pixelFormat() == MTLPixelFormat::R8Unorm)
                    {
                        // Preserve a preceding monochrome atlas if a later update
                        // contains colored glyphs; regular artwork always stays RGBA.
                        let old = &self.textures[&id];
                        let [width, height] = [old.width(), old.height()];
                        let region = MTLRegion {
                            origin: MTLOrigin { x: 0, y: 0, z: 0 },
                            size: MTLSize {
                                width,
                                height,
                                depth: 1,
                            },
                        };
                        let mut coverage = vec![0u8; width * height];
                        old.getBytes_bytesPerRow_fromRegion_mipmapLevel(
                            NonNull::new(coverage.as_mut_ptr().cast()).unwrap(),
                            width,
                            region,
                            0,
                        );
                        let rgba: Vec<u8> = coverage.into_iter().flat_map(|a| [a; 4]).collect();
                        let descriptor =
                            self.texture_descriptor(MTLPixelFormat::RGBA8Unorm, [width, height]);
                        let texture = self
                            .device
                            .newTextureWithDescriptor(&descriptor)
                            .ok_or("Could not expand font texture")?;
                        texture.replaceRegion_mipmapLevel_withBytes_bytesPerRow(
                            region,
                            0,
                            NonNull::new(rgba.as_ptr().cast::<c_void>().cast_mut()).unwrap(),
                            width * 4,
                        );
                        self.textures.insert(id, texture);
                    }
                    let pos = update.pos.unwrap_or([0, 0]);
                    let texture = self
                        .textures
                        .get(&id)
                        .ok_or("Texture update without allocation")?;
                    if pos[0]
                        .checked_add(size[0])
                        .is_none_or(|v| v > texture.width())
                        || pos[1]
                            .checked_add(size[1])
                            .is_none_or(|v| v > texture.height())
                    {
                        return Err("Texture update exceeds its allocation".into());
                    }
                    let coverage: Vec<u8> = if texture.pixelFormat() == MTLPixelFormat::R8Unorm {
                        image.pixels.iter().map(|p| p.a()).collect()
                    } else {
                        Vec::new()
                    };
                    let (pixels, stride) = if coverage.is_empty() {
                        (image.pixels.as_ptr().cast::<c_void>(), size[0] * 4)
                    } else {
                        (coverage.as_ptr().cast::<c_void>(), size[0])
                    };
                    texture.replaceRegion_mipmapLevel_withBytes_bytesPerRow(
                        MTLRegion {
                            origin: MTLOrigin {
                                x: pos[0],
                                y: pos[1],
                                z: 0,
                            },
                            size: MTLSize {
                                width: size[0],
                                height: size[1],
                                depth: 1,
                            },
                        },
                        0,
                        NonNull::new(pixels.cast_mut()).unwrap(),
                        stride,
                    );
                }
            }
            let Some(drawable) = layer.nextDrawable() else {
                for id in delta.free.drain() {
                    self.textures.remove(&id);
                }
                return Ok(None);
            };
            let texture = drawable.texture();
            let width = texture.width();
            let height = texture.height();
            self.vertices.clear();
            self.indices.clear();
            for primitive in primitives {
                if let Primitive::Mesh(mesh) = &primitive.primitive {
                    debug_assert!(mesh.is_valid());
                    self.vertices.extend_from_slice(&mesh.vertices);
                    self.indices.extend_from_slice(&mesh.indices);
                }
            }
            let vbsize = self.vertices.len() * std::mem::size_of::<Vertex>();
            let ibsize = self.indices.len() * 4;
            Self::buffer(&self.device, &mut self.vertex_buffer, vbsize);
            Self::buffer(&self.device, &mut self.index_buffer, ibsize);
            let vb = self
                .vertex_buffer
                .as_ref()
                .ok_or("Could not allocate Metal vertices")?;
            let ib = self
                .index_buffer
                .as_ref()
                .ok_or("Could not allocate Metal indices")?;
            std::ptr::copy_nonoverlapping(
                self.vertices.as_ptr().cast::<u8>(),
                vb.contents().as_ptr().cast::<u8>(),
                vbsize,
            );
            std::ptr::copy_nonoverlapping(
                self.indices.as_ptr().cast::<u8>(),
                ib.contents().as_ptr().cast::<u8>(),
                ibsize,
            );
            let pass = MTLRenderPassDescriptor::new();
            let attachment = pass.colorAttachments().objectAtIndexedSubscript(0);
            attachment.setTexture(Some(&texture));
            attachment.setLoadAction(MTLLoadAction::Clear);
            attachment.setStoreAction(MTLStoreAction::Store);
            attachment.setClearColor(MTLClearColor {
                red: 0.,
                green: 0.,
                blue: 0.,
                alpha: 1.,
            });
            let command = self
                .queue
                .commandBuffer()
                .ok_or("Could not allocate Metal command buffer")?;
            let encoder = command
                .renderCommandEncoderWithDescriptor(&pass)
                .ok_or("Could not create Metal render encoder")?;
            encoder.setRenderPipelineState(&self.pipeline);
            encoder.setViewport(MTLViewport {
                originX: 0.,
                originY: 0.,
                width: width as f64,
                height: height as f64,
                znear: 0.,
                zfar: 1.,
            });
            let screen = [width as f32 / ppp, height as f32 / ppp];
            encoder.setVertexBytes_length_atIndex(NonNull::from(&screen).cast::<c_void>(), 8, 1);
            encoder.setFragmentSamplerState_atIndex(Some(&self.sampler), 0);
            let mut vertex_offset = 0;
            let mut index_offset = 0;
            for primitive in primitives {
                let Primitive::Mesh(mesh) = &primitive.primitive else {
                    continue;
                };
                let rect = primitive.clip_rect;
                let x = (rect.min.x * ppp).round().clamp(0., width as f32) as usize;
                let y = (rect.min.y * ppp).round().clamp(0., height as f32) as usize;
                let right = (rect.max.x * ppp).round().clamp(x as f32, width as f32) as usize;
                let bottom = (rect.max.y * ppp).round().clamp(y as f32, height as f32) as usize;
                if right > x && bottom > y {
                    encoder.setScissorRect(MTLScissorRect {
                        x,
                        y,
                        width: right - x,
                        height: bottom - y,
                    });
                    encoder.setVertexBuffer_offset_atIndex(
                        Some(vb),
                        vertex_offset * std::mem::size_of::<Vertex>(),
                        0,
                    );
                    encoder.setFragmentTexture_atIndex(
                        self.textures.get(&mesh.texture_id).map(|v| v.as_ref()),
                        0,
                    );
                    encoder
                        .drawIndexedPrimitives_indexCount_indexType_indexBuffer_indexBufferOffset(
                            MTLPrimitiveType::Triangle,
                            mesh.indices.len(),
                            MTLIndexType::UInt32,
                            ib,
                            index_offset * 4,
                        );
                }
                vertex_offset += mesh.vertices.len();
                index_offset += mesh.indices.len();
            }
            encoder.endEncoding();
            let stride = (width * 4).next_multiple_of(256);
            let readback = if capture {
                let buffer = self
                    .device
                    .newBufferWithLength_options(
                        stride * height,
                        MTLResourceOptions::StorageModeShared,
                    )
                    .ok_or("Could not allocate screenshot readback")?;
                let blit = command
                    .blitCommandEncoder()
                    .ok_or("Could not encode screenshot readback")?;
                blit.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toBuffer_destinationOffset_destinationBytesPerRow_destinationBytesPerImage(
                    &texture, 0, 0, MTLOrigin { x:0,y:0,z:0 }, MTLSize { width,height,depth:1 }, &buffer, 0, stride, stride * height);
                blit.endEncoding();
                Some(buffer)
            } else {
                None
            };
            command.presentDrawable(ProtocolObject::from_ref(&*drawable));
            command.commit();
            let image = if let Some(buffer) = readback {
                command.waitUntilCompleted();
                if command.status() == MTLCommandBufferStatus::Error {
                    return Err(format!("Metal screenshot failed: {:?}", command.error()));
                }
                let bytes = std::slice::from_raw_parts(
                    buffer.contents().as_ptr().cast::<u8>(),
                    stride * height,
                );
                let pixels =
                    bytes
                        .chunks_exact(stride)
                        .flat_map(|row| {
                            row[..width * 4].as_chunks::<4>().0.iter().map(|p| {
                                egui::Color32::from_rgba_premultiplied(p[2], p[1], p[0], p[3])
                            })
                        })
                        .collect();
                Some(egui::ColorImage::new([width, height], pixels))
            } else {
                None
            };
            // commandBuffer retains every encoded resource until GPU completion.
            // Texture handles can therefore be released immediately after submit.
            self.pending = Some(command);
            for id in delta.free.drain() {
                self.textures.remove(&id);
            }
            Ok(image)
        })
    }
}
impl Drop for Painter {
    fn drop(&mut self) {
        if let Some(command) = self.pending.take() {
            command.waitUntilCompleted();
        }
    }
}
