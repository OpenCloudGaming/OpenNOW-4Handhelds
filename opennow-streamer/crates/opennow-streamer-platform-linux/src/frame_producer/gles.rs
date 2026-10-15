//! OpenGL ES presenter for the handheld EGLFS runtime.
//!
//! Qt makes the GLES context current around `beginExternal`. This module samples
//! NV12 or P010 (CPU planes or a DMA-BUF) into one RGBA texture per QRhi slot.
//! The texture name is returned to Qt; this presenter keeps owning it.
//!
//! OpenGL ES 3 imports a decoder DMA-BUF and reads it on the GPU. NV12 chroma is
//! sampled as individual bytes, because Mali returns only the red channel of an
//! RG texture. OpenGL ES 2, which is the whole GPU on Mali-450-class chips,
//! samples linear NV12 through luminance textures. A tiled DMA-BUF still needs ES 3.

use std::ffi::CStr;
use std::os::raw::{c_char, c_void};
use std::sync::atomic::{AtomicBool, Ordering};

use libloading::Library;

use super::{GpuTextureFormat, LinuxTextureColorSpace, RecordedGpuFrame};
use crate::{
    ColorMatrix, ColorRange, DecodedVideoFrame, DmaBufFrame, DmaBufPlane, Error, FramePlane,
    PixelFormat, Result, Subsystem,
};

const DRM_FORMAT_MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;
const DRM_FORMAT_MOD_LINEAR: u64 = 0;
const DRM_FORMAT_NV12: u32 = u32::from_le_bytes(*b"NV12");
const DRM_FORMAT_P010: u32 = u32::from_le_bytes(*b"P010");
const DRM_FORMAT_R16: u32 = u32::from_le_bytes(*b"R16 ");
const DRM_FORMAT_GR1616: u32 = u32::from_le_bytes(*b"GR32");
const DRM_FORMAT_R8: u32 = u32::from_le_bytes(*b"R8  ");
const DRM_FORMAT_GR88: u32 = u32::from_le_bytes(*b"GR88");

const EGL_NONE: i32 = 0x3038;
const EGL_SUCCESS: i32 = 0x3000;
const EGL_WIDTH: i32 = 0x3057;
const EGL_HEIGHT: i32 = 0x3056;
const EGL_LINUX_DMA_BUF_EXT: u32 = 0x3270;
const EGL_LINUX_DRM_FOURCC_EXT: i32 = 0x3271;
const EGL_DMA_BUF_PLANE0_FD_EXT: i32 = 0x3272;
const EGL_DMA_BUF_PLANE0_OFFSET_EXT: i32 = 0x3273;
const EGL_DMA_BUF_PLANE0_PITCH_EXT: i32 = 0x3274;
const EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT: i32 = 0x3443;
const EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT: i32 = 0x3444;

const GL_TRIANGLES: u32 = 0x0004;
const GL_UNPACK_ALIGNMENT: u32 = 0x0CF5;
const GL_UNPACK_ROW_LENGTH: u32 = 0x0CF2;
const GL_TEXTURE_2D: u32 = 0x0DE1;
const GL_UNSIGNED_BYTE: u32 = 0x1401;
const GL_UNSIGNED_SHORT: u32 = 0x1403;
const GL_FLOAT: u32 = 0x1406;
const GL_RED: u32 = 0x1903;
const GL_LUMINANCE: u32 = 0x1909;
const GL_LUMINANCE_ALPHA: u32 = 0x190A;
const GL_RGBA: u32 = 0x1908;
const GL_VERSION: u32 = 0x1F02;
const GL_ARRAY_BUFFER: u32 = 0x8892;
const GL_STATIC_DRAW: u32 = 0x88E4;
const GL_NEAREST: i32 = 0x2600;
const GL_LINEAR: i32 = 0x2601;
const GL_TEXTURE_MAG_FILTER: u32 = 0x2800;
const GL_TEXTURE_MIN_FILTER: u32 = 0x2801;
const GL_TEXTURE_WRAP_S: u32 = 0x2802;
const GL_TEXTURE_WRAP_T: u32 = 0x2803;
const GL_EXTENSIONS: u32 = 0x1F03;
const GL_CLAMP_TO_EDGE: i32 = 0x812F;
const GL_TEXTURE0: u32 = 0x84C0;
const GL_RGBA8: u32 = 0x8058;
const GL_RGB10_A2: u32 = 0x8059;
const GL_RGBA16F: u32 = 0x881A;
const GL_NUM_EXTENSIONS: u32 = 0x821D;
const GL_R8: u32 = 0x8229;
const GL_R16: u32 = 0x822A;
const GL_RG16: u32 = 0x822C;
const GL_RG: u32 = 0x8227;
const GL_TEXTURE_MAX_LEVEL: u32 = 0x813D;
const GL_FRAGMENT_SHADER: u32 = 0x8B30;
const GL_VERTEX_SHADER: u32 = 0x8B31;
const GL_COMPILE_STATUS: u32 = 0x8B81;
const GL_LINK_STATUS: u32 = 0x8B82;
const GL_INFO_LOG_LENGTH: u32 = 0x8B84;
const GL_FRAMEBUFFER: u32 = 0x8D40;
const GL_COLOR_ATTACHMENT0: u32 = 0x8CE0;
const GL_FRAMEBUFFER_COMPLETE: u32 = 0x8CD5;

const VERTEX_SHADER: &str = r#"#version 300 es
out vec2 texture_coordinates;
void main() {
    const vec2 positions[3] = vec2[3](
        vec2(-1.0, -1.0),
        vec2( 3.0, -1.0),
        vec2(-1.0,  3.0)
    );
    vec2 position = positions[gl_VertexID];
    gl_Position = vec4(position, 0.0, 1.0);
    // GLES NDC is Y-up. v = 0 stays the first stored row, which is the top.
    texture_coordinates = vec2((position.x + 1.0) * 0.5, (1.0 - position.y) * 0.5);
}
"#;

const FRAGMENT_SHADER: &str = r#"#version 300 es
precision highp float;
uniform sampler2D luma_texture;
uniform sampler2D chroma_texture;
uniform vec2 texture_scale;
uniform uint color_matrix;
uniform uint full_range;
uniform uint sample_bits;
uniform float chroma_offset_x;
uniform float chroma_bytes;
uniform float luma_width;
uniform float chroma_pitch;
uniform uint copy_rgba;
in vec2 texture_coordinates;
out vec4 output_color;
void main() {
    vec2 source = texture_coordinates * texture_scale;
    if (copy_rgba == 1u) {
        output_color = texture(luma_texture, source);
        return;
    }
    float y = texture(luma_texture, source).r;
    vec2 uv;
    if (chroma_bytes > 0.5) {
        float pair = floor(source.x * luma_width * 0.5);
        float pitch = max(chroma_pitch, 1.0);
        uv.x = texture(chroma_texture, vec2((pair * 2.0 + 0.5) / pitch, source.y)).r;
        uv.y = texture(chroma_texture, vec2((pair * 2.0 + 1.5) / pitch, source.y)).r;
    } else {
        uv = texture(chroma_texture, source + vec2(chroma_offset_x, 0.0)).rg;
    }
    bool ten_bit = sample_bits != 8u;
    if (sample_bits == 16u) {
        y *= 65535.0 / 65472.0;
        uv *= 65535.0 / 65472.0;
    }
    float maximum = ten_bit ? 1023.0 : 255.0;
    float multiplier = ten_bit ? 4.0 : 1.0;
    float midpoint = 128.0 * multiplier / maximum;
    float u = uv.x - midpoint;
    float v = uv.y - midpoint;
    if (full_range == 0u) {
        y = max(0.0, y - 16.0 * multiplier / maximum) * maximum / (219.0 * multiplier);
        u *= maximum / (224.0 * multiplier);
        v *= maximum / (224.0 * multiplier);
    }
    vec3 rgb;
    if (color_matrix == 0u) {
        rgb = vec3(y + 1.402 * v, y - 0.344136 * u - 0.714136 * v, y + 1.772 * u);
    } else if (color_matrix == 2u) {
        rgb = vec3(y + 1.4746 * v, y - 0.164553 * u - 0.571353 * v, y + 1.8814 * u);
    } else {
        rgb = vec3(y + 1.5748 * v, y - 0.187324 * u - 0.468124 * v, y + 1.8556 * u);
    }
    output_color = vec4(clamp(rgb, 0.0, 1.0), 1.0);
}
"#;

const ES2_VERTEX_SHADER: &str = r#"#version 100
attribute vec2 position;
attribute vec2 texture_coordinates;
varying vec2 interpolated_coordinates;
void main() {
    gl_Position = vec4(position, 0.0, 1.0);
    interpolated_coordinates = texture_coordinates;
}
"#;

const ES2_FRAGMENT_SHADER: &str = r#"#version 100
precision highp float;
uniform sampler2D luma_texture;
uniform sampler2D chroma_texture;
varying vec2 interpolated_coordinates;
uniform vec2 texture_scale;
uniform float color_matrix;
uniform float full_range;
uniform float sample_bits;
uniform float chroma_offset_x;
uniform float chroma_bytes;
uniform float luma_width;
uniform float chroma_pitch;
uniform float copy_rgba;
void main() {
    vec2 source = interpolated_coordinates * texture_scale;
    if (copy_rgba > 0.5) {
        gl_FragColor = texture2D(luma_texture, source);
        return;
    }
    float y = texture2D(luma_texture, source).r;
    vec2 uv;
    if (chroma_bytes > 0.5) {
        float pair = floor(source.x * luma_width * 0.5);
        float pitch = max(chroma_pitch, 1.0);
        uv.x = texture2D(chroma_texture, vec2((pair * 2.0 + 0.5) / pitch, source.y)).r;
        uv.y = texture2D(chroma_texture, vec2((pair * 2.0 + 1.5) / pitch, source.y)).r;
    } else {
        vec4 chroma = texture2D(chroma_texture, source + vec2(chroma_offset_x, 0.0));
        uv = vec2(chroma.r, chroma.a);
    }
    bool ten_bit = sample_bits > 8.5;
    if (sample_bits > 15.5) {
        y *= 65535.0 / 65472.0;
        uv *= 65535.0 / 65472.0;
    }
    float maximum = ten_bit ? 1023.0 : 255.0;
    float multiplier = ten_bit ? 4.0 : 1.0;
    float midpoint = 128.0 * multiplier / maximum;
    float u = uv.x - midpoint;
    float v = uv.y - midpoint;
    if (full_range < 0.5) {
        y = max(0.0, y - 16.0 * multiplier / maximum) * maximum / (219.0 * multiplier);
        u *= maximum / (224.0 * multiplier);
        v *= maximum / (224.0 * multiplier);
    }
    vec3 rgb;
    if (color_matrix < 0.5) {
        rgb = vec3(y + 1.402 * v, y - 0.344136 * u - 0.714136 * v, y + 1.772 * u);
    } else if (color_matrix > 1.5) {
        rgb = vec3(y + 1.4746 * v, y - 0.164553 * u - 0.571353 * v, y + 1.8814 * u);
    } else {
        rgb = vec3(y + 1.5748 * v, y - 0.187324 * u - 0.468124 * v, y + 1.8556 * u);
    }
    gl_FragColor = vec4(clamp(rgb, 0.0, 1.0), 1.0);
}
"#;

/// Fullscreen triangle. UVs match the ES 3 shader, which flips Y so the first
/// stored row stays at the top of the picture.
const ES2_VERTICES: [f32; 12] = [
    -1.0, -1.0, 0.0, 1.0, 3.0, -1.0, 2.0, 1.0, -1.0, 3.0, 0.0, -1.0,
];

type GenObjects = unsafe extern "C" fn(i32, *mut u32);
type DeleteObjects = unsafe extern "C" fn(i32, *const u32);
type Bind1 = unsafe extern "C" fn(u32, u32);
type TexImage = unsafe extern "C" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void);
type TexStorage = unsafe extern "C" fn(u32, i32, u32, i32, i32);
type BufferData = unsafe extern "C" fn(u32, isize, *const c_void, u32);
type EnableAttrib = unsafe extern "C" fn(u32);
type AttribPointer = unsafe extern "C" fn(u32, i32, u32, u8, i32, *const c_void);
type BindAttrib = unsafe extern "C" fn(u32, u32, *const c_char);
type GetString = unsafe extern "C" fn(u32) -> *const u8;
type TexParam = unsafe extern "C" fn(u32, u32, i32);
type PixelStore = unsafe extern "C" fn(u32, i32);
type FramebufferTexture = unsafe extern "C" fn(u32, u32, u32, u32, i32);
type CheckStatus = unsafe extern "C" fn(u32) -> u32;
type Viewport = unsafe extern "C" fn(i32, i32, i32, i32);
type UseProgram = unsafe extern "C" fn(u32);
type DrawArrays = unsafe extern "C" fn(u32, i32, i32);
type Finish = unsafe extern "C" fn();
type CreateShader = unsafe extern "C" fn(u32) -> u32;
type ShaderSource = unsafe extern "C" fn(u32, i32, *const *const c_char, *const i32);
type ShaderIv = unsafe extern "C" fn(u32, u32, *mut i32);
type ShaderLog = unsafe extern "C" fn(u32, i32, *mut i32, *mut c_char);
type CreateProgram = unsafe extern "C" fn() -> u32;
type AttachShader = unsafe extern "C" fn(u32, u32);
type LinkProgram = unsafe extern "C" fn(u32);
type DeleteShader = unsafe extern "C" fn(u32);
type DeleteProgram = unsafe extern "C" fn(u32);
type GetUniform = unsafe extern "C" fn(u32, *const c_char) -> i32;
type Uniform1i = unsafe extern "C" fn(i32, i32);
type Uniform1ui = unsafe extern "C" fn(i32, u32);
type Uniform1f = unsafe extern "C" fn(i32, f32);
type Uniform2f = unsafe extern "C" fn(i32, f32, f32);
type ActiveTexture = unsafe extern "C" fn(u32);
type GetError = unsafe extern "C" fn() -> u32;
type GetIntegerv = unsafe extern "C" fn(u32, *mut i32);
type GetStringi = unsafe extern "C" fn(u32, u32) -> *const u8;
type EglProc = unsafe extern "C" fn(*const c_char) -> *mut c_void;
type EglDisplayFn = unsafe extern "C" fn() -> *mut c_void;
type EglErrorFn = unsafe extern "C" fn() -> i32;
type EglCreateImage = unsafe extern "C" fn(
    *mut c_void,
    *mut c_void,
    u32,
    *mut c_void,
    *const i32,
) -> *mut c_void;
type EglDestroyImage = unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32;
type ImageTarget = unsafe extern "C" fn(u32, *mut c_void);

struct Gl {
    _gles: Library,
    _egl: Library,
    gen_textures: GenObjects,
    delete_textures: DeleteObjects,
    bind_texture: Bind1,
    tex_image: TexImage,
    tex_storage: Option<TexStorage>,
    tex_parameteri: TexParam,
    pixel_storei: PixelStore,
    gen_framebuffers: GenObjects,
    delete_framebuffers: DeleteObjects,
    bind_framebuffer: Bind1,
    framebuffer_texture: FramebufferTexture,
    check_framebuffer: CheckStatus,
    viewport: Viewport,
    use_program: UseProgram,
    draw_arrays: DrawArrays,
    finish: Option<Finish>,
    create_shader: CreateShader,
    shader_source: ShaderSource,
    get_shaderiv: ShaderIv,
    get_shader_log: ShaderLog,
    create_program: CreateProgram,
    attach_shader: AttachShader,
    link_program: LinkProgram,
    get_programiv: ShaderIv,
    get_program_log: ShaderLog,
    delete_shader: DeleteShader,
    delete_program: DeleteProgram,
    get_uniform: GetUniform,
    uniform_1i: Uniform1i,
    uniform_1ui: Option<Uniform1ui>,
    uniform_1f: Uniform1f,
    uniform_2f: Uniform2f,
    active_texture: ActiveTexture,
    get_error: GetError,
    get_integerv: GetIntegerv,
    get_string: GetString,
    get_stringi: Option<GetStringi>,
    gen_buffers: GenObjects,
    delete_buffers: DeleteObjects,
    bind_buffer: Bind1,
    buffer_data: BufferData,
    enable_attrib: EnableAttrib,
    attrib_pointer: AttribPointer,
    bind_attrib: BindAttrib,
    get_current_display: EglDisplayFn,
    get_current_context: EglDisplayFn,
    get_egl_error: EglErrorFn,
    create_image: Option<EglCreateImage>,
    destroy_image: Option<EglDestroyImage>,
    image_target: Option<ImageTarget>,
}

struct Program {
    name: u32,
    luma: i32,
    chroma: i32,
    texture_scale: i32,
    color_matrix: i32,
    full_range: i32,
    sample_bits: i32,
    chroma_offset_x: i32,
    chroma_bytes: i32,
    luma_width: i32,
    chroma_pitch: i32,
    copy_rgba: i32,
    vertex_buffer: u32,
}

struct PlaneSample {
    scale_y: f32,
    sample_bits: u32,
    chroma_offset_x: f32,
    byte_chroma: bool,
    luma_width: f32,
    chroma_pitch: f32,
}

struct Slot {
    output: u32,
    framebuffer: u32,
    luma: u32,
    chroma: u32,
    luma_image: usize,
    chroma_image: usize,
    width: u32,
    height: u32,
    format: GpuTextureFormat,
    retained: Option<DecodedVideoFrame>,
}

pub(super) struct GlesPresenter {
    gl: Gl,
    program: Program,
    slots: Vec<Option<Slot>>,
    generation: u64,
    norm16: bool,
    gles3: bool,
    destroyed: bool,
}

impl Gl {
    fn load() -> Result<Self> {
        let gles = open_library(&["libGLESv2.so.2", "libGLESv2.so"])?;
        let egl = open_library(&["libEGL.so.1", "libEGL.so"])?;
        let get_proc: EglProc = load_symbol(&egl, b"eglGetProcAddress\0")?;
        let create_image = proc_symbol(&get_proc, b"eglCreateImageKHR\0");
        let destroy_image = proc_symbol(&get_proc, b"eglDestroyImageKHR\0");
        let image_target = proc_symbol(&get_proc, b"glEGLImageTargetTexture2DOES\0");
        Ok(Self {
            gen_textures: load_symbol(&gles, b"glGenTextures\0")?,
            delete_textures: load_symbol(&gles, b"glDeleteTextures\0")?,
            bind_texture: load_symbol(&gles, b"glBindTexture\0")?,
            tex_image: load_symbol(&gles, b"glTexImage2D\0")?,
            tex_storage: load_symbol(&gles, b"glTexStorage2D\0").ok(),
            tex_parameteri: load_symbol(&gles, b"glTexParameteri\0")?,
            pixel_storei: load_symbol(&gles, b"glPixelStorei\0")?,
            gen_framebuffers: load_symbol(&gles, b"glGenFramebuffers\0")?,
            delete_framebuffers: load_symbol(&gles, b"glDeleteFramebuffers\0")?,
            bind_framebuffer: load_symbol(&gles, b"glBindFramebuffer\0")?,
            framebuffer_texture: load_symbol(&gles, b"glFramebufferTexture2D\0")?,
            check_framebuffer: load_symbol(&gles, b"glCheckFramebufferStatus\0")?,
            viewport: load_symbol(&gles, b"glViewport\0")?,
            use_program: load_symbol(&gles, b"glUseProgram\0")?,
            draw_arrays: load_symbol(&gles, b"glDrawArrays\0")?,
            finish: load_symbol(&gles, b"glFinish\0").ok(),
            create_shader: load_symbol(&gles, b"glCreateShader\0")?,
            shader_source: load_symbol(&gles, b"glShaderSource\0")?,
            get_shaderiv: load_symbol(&gles, b"glGetShaderiv\0")?,
            get_shader_log: load_symbol(&gles, b"glGetShaderInfoLog\0")?,
            create_program: load_symbol(&gles, b"glCreateProgram\0")?,
            attach_shader: load_symbol(&gles, b"glAttachShader\0")?,
            link_program: load_symbol(&gles, b"glLinkProgram\0")?,
            get_programiv: load_symbol(&gles, b"glGetProgramiv\0")?,
            get_program_log: load_symbol(&gles, b"glGetProgramInfoLog\0")?,
            delete_shader: load_symbol(&gles, b"glDeleteShader\0")?,
            delete_program: load_symbol(&gles, b"glDeleteProgram\0")?,
            get_uniform: load_symbol(&gles, b"glGetUniformLocation\0")?,
            uniform_1i: load_symbol(&gles, b"glUniform1i\0")?,
            uniform_1ui: load_symbol(&gles, b"glUniform1ui\0").ok(),
            uniform_1f: load_symbol(&gles, b"glUniform1f\0")?,
            uniform_2f: load_symbol(&gles, b"glUniform2f\0")?,
            active_texture: load_symbol(&gles, b"glActiveTexture\0")?,
            get_error: load_symbol(&gles, b"glGetError\0")?,
            get_integerv: load_symbol(&gles, b"glGetIntegerv\0")?,
            get_string: load_symbol(&gles, b"glGetString\0")?,
            get_stringi: load_symbol(&gles, b"glGetStringi\0").ok(),
            gen_buffers: load_symbol(&gles, b"glGenBuffers\0")?,
            delete_buffers: load_symbol(&gles, b"glDeleteBuffers\0")?,
            bind_buffer: load_symbol(&gles, b"glBindBuffer\0")?,
            buffer_data: load_symbol(&gles, b"glBufferData\0")?,
            enable_attrib: load_symbol(&gles, b"glEnableVertexAttribArray\0")?,
            attrib_pointer: load_symbol(&gles, b"glVertexAttribPointer\0")?,
            bind_attrib: load_symbol(&gles, b"glBindAttribLocation\0")?,
            get_current_display: load_symbol(&egl, b"eglGetCurrentDisplay\0")?,
            get_current_context: load_symbol(&egl, b"eglGetCurrentContext\0")?,
            get_egl_error: load_symbol(&egl, b"eglGetError\0")?,
            create_image,
            destroy_image,
            image_target,
            _gles: gles,
            _egl: egl,
        })
    }

    fn context_is_current(&self) -> bool {
        !unsafe { (self.get_current_context)() }.is_null()
    }

    fn clear_error(&self) {
        for _ in 0..8 {
            if unsafe { (self.get_error)() } == 0 {
                break;
            }
        }
    }

    fn check(&self, action: &str) -> Result<()> {
        let error = unsafe { (self.get_error)() };
        if error == 0 {
            return Ok(());
        }
        Err(Error::backend(
            Subsystem::OpenGlEs,
            format!("{action} failed with GL error {error:#x}"),
        ))
    }

    fn has_extension(&self, name: &str) -> bool {
        if let Some(get_stringi) = self.get_stringi {
            let mut count = 0;
            unsafe { (self.get_integerv)(GL_NUM_EXTENSIONS, &mut count) };
            if unsafe { (self.get_error)() } == 0 && count > 0 {
                for index in 0..count as u32 {
                    let value = unsafe { get_stringi(GL_EXTENSIONS, index) };
                    if value.is_null() {
                        continue;
                    }
                    let text = unsafe { CStr::from_ptr(value.cast()) };
                    if text.to_bytes() == name.as_bytes() {
                        return true;
                    }
                }
                return false;
            }
        }
        let value = unsafe { (self.get_string)(GL_EXTENSIONS) };
        if value.is_null() {
            return false;
        }
        let text = unsafe { CStr::from_ptr(value.cast()) };
        text.to_bytes()
            .split(|byte| *byte == b' ')
            .any(|extension| extension == name.as_bytes())
    }

    fn context_is_gles3(&self) -> bool {
        if self.tex_storage.is_none() || self.uniform_1ui.is_none() {
            return false;
        }
        let value = unsafe { (self.get_string)(GL_VERSION) };
        if value.is_null() {
            return false;
        }
        let text = unsafe { CStr::from_ptr(value.cast()) };
        text.to_bytes().windows(11).any(|window| window == b"OpenGL ES 3")
    }
}

impl GlesPresenter {
    pub(super) fn new(slot_count: u32) -> Result<Self> {
        let gl = Gl::load()?;
        if !gl.context_is_current() {
            return Err(Error::unavailable(
                Subsystem::OpenGlEs,
                "Qt must make the EGL context current before the first video frame",
            ));
        }
        gl.clear_error();
        let gles3 = gl.context_is_gles3();
        let program = link_program(&gl, gles3)?;
        let norm16 = gles3 && gl.has_extension("GL_EXT_texture_norm16");
        Ok(Self {
            gl,
            program,
            slots: (0..slot_count).map(|_| None).collect(),
            generation: 0,
            norm16,
            gles3,
            destroyed: false,
        })
    }

    pub(super) fn record(
        &mut self,
        frame: &DecodedVideoFrame,
        slot: u32,
    ) -> Result<RecordedGpuFrame> {
        if self.destroyed || !self.gl.context_is_current() {
            return Err(Error::unavailable(
                Subsystem::OpenGlEs,
                "the EGL context is no longer current on the render thread",
            ));
        }
        frame.validate()?;
        let color_space = LinuxTextureColorSpace::from_format(frame.format)?;
        let texture_format = output_format(frame, color_space)?;
        let slot_index = usize::try_from(slot)
            .ok()
            .filter(|index| *index < self.slots.len())
            .ok_or_else(|| Error::InvalidFormat(format!("invalid QRhi frame slot {slot}")))?;
        self.ensure_slot(
            slot_index,
            frame.format.width,
            frame.format.height,
            texture_format,
        )?;
        let copy_rgba = matches!(
            frame.format.pixel_format,
            PixelFormat::Rgba8 | PixelFormat::Bgra8
        );
        let sample = if copy_rgba {
            self.upload_packed(slot_index, frame)?
        } else if frame.dmabuf.is_some() {
            if self.gles3 {
                match self.import_dmabuf(slot_index, frame) {
                    Ok(sample) => sample,
                    Err(error) => {
                        static LOGGED_IMPORT: AtomicBool = AtomicBool::new(false);
                        if LOGGED_IMPORT
                            .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
                            .is_ok()
                        {
                            eprintln!(
                                "stream: GPU import of the decoder picture failed ({error}); copying it instead"
                            );
                        }
                        let (luma, chroma) = map_linear_nv12(frame)?;
                        self.upload_nv12_planes(slot_index, frame, &luma, &chroma)?
                    }
                }
            } else {
                let (luma, chroma) = map_linear_nv12(frame)?;
                self.upload_nv12_planes(slot_index, frame, &luma, &chroma)?
            }
        } else if let Some(vulkan) = frame.vulkan.as_ref() {
            let planes = vulkan.download_nv12().map_err(|_| {
                Error::unavailable(
                    Subsystem::OpenGlEs,
                    "a Vulkan decoded frame has no CPU picture for OpenGL ES",
                )
            })?;
            if planes.len() != 2 {
                return Err(Error::InvalidFormat(
                    "Vulkan CPU fallback did not produce NV12 planes".to_owned(),
                ));
            }
            self.upload_nv12_planes(slot_index, frame, &planes[0], &planes[1])?
        } else {
            self.upload_cpu(slot_index, frame)?
        };
        self.draw(slot_index, frame, &sample, copy_rgba)?;
        self.generation = self.generation.wrapping_add(1);
        let resources = self.slots[slot_index].as_ref().expect("slot created");
        Ok(RecordedGpuFrame {
            slot,
            generation: self.generation,
            image: u64::from(resources.output),
            image_view: 0,
            texture_format,
            width: frame.format.width,
            height: frame.format.height,
            timestamp_us: frame.timestamp_us,
            color_space,
        })
    }

    fn ensure_slot(
        &mut self,
        index: usize,
        width: u32,
        height: u32,
        format: GpuTextureFormat,
    ) -> Result<()> {
        let matches = self.slots[index].as_ref().is_some_and(|slot| {
            slot.width == width && slot.height == height && slot.format == format
        });
        if matches {
            return Ok(());
        }
        if let Some(slot) = self.slots[index].take() {
            self.destroy_slot(slot);
        }
        if !self.gles3 && format != GpuTextureFormat::Rgba8 {
            return Err(Error::unavailable(
                Subsystem::OpenGlEs,
                "OpenGL ES 2 presents 8-bit SDR only",
            ));
        }
        let internal = match format {
            GpuTextureFormat::Rgba8 => GL_RGBA8,
            GpuTextureFormat::Rgb10A2 => GL_RGB10_A2,
            GpuTextureFormat::Rgba16Float => GL_RGBA16F,
        };
        self.gl.clear_error();
        let mut output = 0;
        let mut framebuffer = 0;
        let mut luma = 0;
        let mut chroma = 0;
        unsafe {
            (self.gl.gen_textures)(1, &mut output);
            (self.gl.gen_textures)(1, &mut luma);
            (self.gl.gen_textures)(1, &mut chroma);
            (self.gl.gen_framebuffers)(1, &mut framebuffer);
            (self.gl.bind_texture)(GL_TEXTURE_2D, output);
            if let Some(tex_storage) = self.gles3.then_some(self.gl.tex_storage).flatten() {
                tex_storage(GL_TEXTURE_2D, 1, internal, width as i32, height as i32);
            } else {
                (self.gl.tex_image)(
                    GL_TEXTURE_2D,
                    0,
                    GL_RGBA as i32,
                    width as i32,
                    height as i32,
                    0,
                    GL_RGBA,
                    GL_UNSIGNED_BYTE,
                    std::ptr::null(),
                );
            }
            (self.gl.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
            (self.gl.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
            (self.gl.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
            (self.gl.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
            (self.gl.bind_framebuffer)(GL_FRAMEBUFFER, framebuffer);
            (self.gl.framebuffer_texture)(
                GL_FRAMEBUFFER,
                GL_COLOR_ATTACHMENT0,
                GL_TEXTURE_2D,
                output,
                0,
            );
        }
        let created = self.gl.check("create the OpenGL ES video target");
        let status = unsafe { (self.gl.check_framebuffer)(GL_FRAMEBUFFER) };
        unsafe { (self.gl.bind_framebuffer)(GL_FRAMEBUFFER, 0) };
        if created.is_err() || output == 0 || status != GL_FRAMEBUFFER_COMPLETE {
            self.destroy_slot(Slot {
                output,
                framebuffer,
                luma,
                chroma,
                luma_image: 0,
                chroma_image: 0,
                width,
                height,
                format,
                retained: None,
            });
            created?;
            return Err(Error::unavailable(
                Subsystem::OpenGlEs,
                format!("video framebuffer is incomplete ({status:#x})"),
            ));
        }
        self.slots[index] = Some(Slot {
            output,
            framebuffer,
            luma,
            chroma,
            luma_image: 0,
            chroma_image: 0,
            width,
            height,
            format,
            retained: None,
        });
        Ok(())
    }

    fn import_dmabuf(
        &mut self,
        index: usize,
        frame: &DecodedVideoFrame,
    ) -> Result<PlaneSample> {
        let dmabuf = frame.dmabuf.as_ref().expect("dma-buf frame");
        let (luma, chroma) = yuv_planes(dmabuf, frame.format.pixel_format)?;
        let coded_height =
            crate::format::nv12_coded_height(&luma, &chroma, frame.format.height).max(1);
        let p010 = frame.format.pixel_format == PixelFormat::P010;
        if p010 && !self.norm16 {
            return Err(Error::unavailable(
                Subsystem::OpenGlEs,
                "P010 needs GL_EXT_texture_norm16",
            ));
        }
        let luma_image = self.create_image(
            dmabuf,
            &luma,
            if p010 { DRM_FORMAT_R16 } else { DRM_FORMAT_R8 },
            frame.format.width,
            coded_height,
        )?;
        let chroma_rows = (coded_height / 2).max(1);
        let (chroma_image, byte_chroma) = if p010 {
            match self.create_image(
                dmabuf,
                &chroma,
                DRM_FORMAT_GR1616,
                frame.format.width / 2,
                chroma_rows,
            ) {
                Ok(image) => (image, false),
                Err(error) => {
                    self.destroy_one_image(luma_image);
                    return Err(error);
                }
            }
        } else {
            match self.create_image(
                dmabuf,
                &chroma,
                DRM_FORMAT_R8,
                u32::try_from(chroma.pitch).unwrap_or(u32::MAX),
                chroma_rows,
            ) {
                Ok(image) => (image, true),
                Err(error) => {
                    self.destroy_one_image(luma_image);
                    return Err(error);
                }
            }
        };
        let (luma_texture, chroma_texture) = {
            let slot = self.slots[index].as_ref().expect("slot created");
            (slot.luma, slot.chroma)
        };
        self.destroy_images_at(index);
        {
            let slot = self.slots[index].as_mut().expect("slot created");
            slot.luma_image = luma_image as usize;
            slot.chroma_image = chroma_image as usize;
            slot.retained = Some(frame.clone());
        }
        self.bind_image(luma_texture, luma_image as usize)?;
        self.bind_image(chroma_texture, chroma_image as usize)?;
        if byte_chroma {
            self.nearest(chroma_texture);
        }
        let mut sample = conversion_inputs(frame, coded_height);
        sample.byte_chroma = byte_chroma;
        if byte_chroma {
            sample.chroma_pitch = chroma.pitch as f32;
        }
        Ok(sample)
    }

    fn create_image(
        &self,
        frame: &DmaBufFrame,
        plane: &DmaBufPlane,
        fourcc: u32,
        width: u32,
        height: u32,
    ) -> Result<*mut c_void> {
        let create = self.gl.create_image.ok_or_else(|| {
            Error::unavailable(
                Subsystem::OpenGlEs,
                "eglCreateImageKHR is not available for DMA-BUF import",
            )
        })?;
        let object = frame.objects.get(plane.object_index).ok_or_else(|| {
            Error::InvalidFormat("DMA-BUF plane object is missing".to_owned())
        })?;
        let mut attribs = vec![
            EGL_WIDTH,
            width as i32,
            EGL_HEIGHT,
            height as i32,
            EGL_LINUX_DRM_FOURCC_EXT,
            fourcc as i32,
            EGL_DMA_BUF_PLANE0_FD_EXT,
            object.fd,
            EGL_DMA_BUF_PLANE0_OFFSET_EXT,
            i32::try_from(plane.offset).unwrap_or(i32::MAX),
            EGL_DMA_BUF_PLANE0_PITCH_EXT,
            i32::try_from(plane.pitch).unwrap_or(i32::MAX),
        ];
        if object.format_modifier != DRM_FORMAT_MOD_INVALID {
            attribs.extend([
                EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT,
                object.format_modifier as u32 as i32,
                EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT,
                (object.format_modifier >> 32) as u32 as i32,
            ]);
        }
        attribs.push(EGL_NONE);
        let display = unsafe { (self.gl.get_current_display)() };
        if display.is_null() {
            return Err(Error::unavailable(
                Subsystem::OpenGlEs,
                "EGL has no current display for DMA-BUF import",
            ));
        }
        let image = unsafe {
            create(
                display,
                std::ptr::null_mut(),
                EGL_LINUX_DMA_BUF_EXT,
                std::ptr::null_mut(),
                attribs.as_ptr(),
            )
        };
        if image.is_null() {
            let error = unsafe { (self.gl.get_egl_error)() };
            return Err(Error::backend(
                Subsystem::OpenGlEs,
                format!("eglCreateImageKHR failed ({error:#x})"),
            ));
        }
        if unsafe { (self.gl.get_egl_error)() } != EGL_SUCCESS {
            if let Some(destroy) = self.gl.destroy_image {
                unsafe { destroy(display, image) };
            }
            return Err(Error::backend(
                Subsystem::OpenGlEs,
                "EGL reported an error after creating a DMA-BUF image",
            ));
        }
        Ok(image)
    }

    fn bind_image(&self, texture: u32, image: usize) -> Result<()> {
        let target = self.gl.image_target.ok_or_else(|| {
            Error::unavailable(
                Subsystem::OpenGlEs,
                "glEGLImageTargetTexture2DOES is not available",
            )
        })?;
        self.gl.clear_error();
        unsafe {
            (self.gl.bind_texture)(GL_TEXTURE_2D, texture);
            target(GL_TEXTURE_2D, image as *mut c_void);
            (self.gl.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
            (self.gl.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
            (self.gl.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
            (self.gl.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
            (self.gl.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MAX_LEVEL, 0);
        }
        self.gl.check("bind the DMA-BUF as an OpenGL ES texture")
    }

    fn upload_cpu(
        &mut self,
        index: usize,
        frame: &DecodedVideoFrame,
    ) -> Result<PlaneSample> {
        match frame.format.pixel_format {
            PixelFormat::Nv12 | PixelFormat::P010 => {
                if frame.planes.len() != 2 {
                    return Err(Error::InvalidFormat(
                        "NV12/P010 frames require two planes".to_owned(),
                    ));
                }
                self.upload_nv12_planes(index, frame, &frame.planes[0], &frame.planes[1])
            }
            PixelFormat::I420 => {
                if frame.planes.len() != 3 {
                    return Err(Error::InvalidFormat(
                        "I420 frames require three planes".to_owned(),
                    ));
                }
                let mut interleaved = Vec::new();
                interleave_i420(
                    &frame.planes[1],
                    &frame.planes[2],
                    frame.format.width as usize,
                    frame.format.height as usize,
                    &mut interleaved,
                );
                let chroma = FramePlane {
                    data: std::sync::Arc::<[u8]>::from(interleaved),
                    stride: frame.format.width as usize,
                    rows: frame.format.height as usize / 2,
                };
                self.upload_nv12_planes(index, frame, &frame.planes[0], &chroma)
            }
            PixelFormat::Nv24 | PixelFormat::P410 => Err(Error::unavailable(
                Subsystem::OpenGlEs,
                "OpenGL ES presentation does not sample 4:4:4 yet",
            )),
            PixelFormat::Rgba8 | PixelFormat::Bgra8 => self.upload_packed(index, frame),
        }
    }

    fn upload_nv12_planes(
        &mut self,
        index: usize,
        frame: &DecodedVideoFrame,
        luma: &FramePlane,
        chroma: &FramePlane,
    ) -> Result<PlaneSample> {
        let p010 = frame.format.pixel_format == PixelFormat::P010;
        if p010 && !self.norm16 {
            return Err(Error::unavailable(
                Subsystem::OpenGlEs,
                "P010 needs GL_EXT_texture_norm16",
            ));
        }
        self.destroy_images_at(index);
        let (luma_texture, chroma_texture) = {
            let slot = self.slots[index].as_mut().expect("slot created");
            slot.retained = Some(frame.clone());
            (slot.luma, slot.chroma)
        };
        let luma_bytes = if p010 { 2 } else { 1 };
        let (luma_internal, luma_format) = if !self.gles3 {
            (GL_LUMINANCE, GL_LUMINANCE)
        } else if p010 {
            (GL_R16, GL_RED)
        } else {
            (GL_R8, GL_RED)
        };
        self.upload_plane(
            luma_texture,
            luma_internal,
            luma_format,
            if p010 { GL_UNSIGNED_SHORT } else { GL_UNSIGNED_BYTE },
            frame.format.width,
            luma.rows as u32,
            luma.stride,
            luma_bytes,
            luma.data.as_ref(),
        )?;
        let byte_chroma = self.gles3 && !p010;
        if byte_chroma {
            let pitch = u32::try_from(chroma.stride).map_err(|_| {
                Error::InvalidFormat("chroma stride does not fit a GPU texture".to_owned())
            })?;
            self.upload_plane(
                chroma_texture,
                GL_R8,
                GL_RED,
                GL_UNSIGNED_BYTE,
                pitch,
                chroma.rows as u32,
                chroma.stride,
                1,
                chroma.data.as_ref(),
            )?;
            self.nearest(chroma_texture);
        } else {
            let (chroma_internal, chroma_format, chroma_bytes) = if p010 {
                (GL_RG16, GL_RG, 4)
            } else {
                (GL_LUMINANCE_ALPHA, GL_LUMINANCE_ALPHA, 2)
            };
            self.upload_plane(
                chroma_texture,
                chroma_internal,
                chroma_format,
                if p010 { GL_UNSIGNED_SHORT } else { GL_UNSIGNED_BYTE },
                frame.format.width / 2,
                chroma.rows as u32,
                chroma.stride,
                chroma_bytes,
                chroma.data.as_ref(),
            )?;
        }
        let coded = u32::try_from(luma.rows).unwrap_or(frame.format.height);
        let mut sample = conversion_inputs(frame, coded.max(1));
        sample.byte_chroma = byte_chroma;
        if byte_chroma {
            sample.chroma_pitch = chroma.stride as f32;
        }
        Ok(sample)
    }

    fn upload_packed(
        &mut self,
        index: usize,
        frame: &DecodedVideoFrame,
    ) -> Result<PlaneSample> {
        if frame.planes.len() != 1 {
            return Err(Error::InvalidFormat(
                "packed RGB frames require one plane".to_owned(),
            ));
        }
        let plane = &frame.planes[0];
        let mut swizzled = Vec::new();
        let bgra = frame.format.pixel_format == PixelFormat::Bgra8;
        if bgra {
            swizzle_bgra(
                plane,
                frame.format.width as usize,
                frame.format.height as usize,
                &mut swizzled,
            );
        }
        let pixels: &[u8] = if bgra { &swizzled } else { plane.data.as_ref() };
        let stride = if bgra {
            frame.format.width as usize * 4
        } else {
            plane.stride
        };
        let rows = if bgra {
            frame.format.height
        } else {
            plane.rows as u32
        };
        self.destroy_images_at(index);
        let luma_texture = {
            let slot = self.slots[index].as_mut().expect("slot created");
            slot.retained = Some(frame.clone());
            slot.luma
        };
        let internal = if self.gles3 { GL_RGBA8 } else { GL_RGBA };
        self.upload_plane(
            luma_texture,
            internal,
            GL_RGBA,
            GL_UNSIGNED_BYTE,
            frame.format.width,
            rows,
            stride,
            4,
            pixels,
        )?;
        Ok(PlaneSample {
            scale_y: 1.0,
            sample_bits: 8,
            chroma_offset_x: 0.0,
            byte_chroma: false,
            luma_width: 1.0,
            chroma_pitch: 1.0,
        })
    }

    fn upload_plane(
        &self,
        texture: u32,
        internal: u32,
        format: u32,
        kind: u32,
        width: u32,
        height: u32,
        stride: usize,
        bytes_per_pixel: usize,
        data: &[u8],
    ) -> Result<()> {
        if bytes_per_pixel == 0 || stride % bytes_per_pixel != 0 {
            return Err(Error::InvalidFormat(
                "plane stride is not a whole number of pixels".to_owned(),
            ));
        }
        let row_pixels = stride / bytes_per_pixel;
        let packed = if !self.gles3 && row_pixels != width as usize {
            Some(pack_rows(
                data,
                stride,
                width as usize * bytes_per_pixel,
                height as usize,
            )?)
        } else {
            None
        };
        let pixels = packed.as_deref().unwrap_or(data);
        self.gl.clear_error();
        unsafe {
            (self.gl.pixel_storei)(GL_UNPACK_ALIGNMENT, 1);
            if self.gles3 {
                (self.gl.pixel_storei)(
                    GL_UNPACK_ROW_LENGTH,
                    i32::try_from(row_pixels).unwrap_or(i32::MAX),
                );
            }
            (self.gl.bind_texture)(GL_TEXTURE_2D, texture);
            (self.gl.tex_image)(
                GL_TEXTURE_2D,
                0,
                internal as i32,
                width as i32,
                height as i32,
                0,
                format,
                kind,
                pixels.as_ptr().cast(),
            );
            (self.gl.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
            (self.gl.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
            (self.gl.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
            (self.gl.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
            if self.gles3 {
                (self.gl.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MAX_LEVEL, 0);
                (self.gl.pixel_storei)(GL_UNPACK_ROW_LENGTH, 0);
            }
        }
        self.gl.check("upload a video plane to OpenGL ES")
    }

    fn nearest(&self, texture: u32) {
        unsafe {
            (self.gl.bind_texture)(GL_TEXTURE_2D, texture);
            (self.gl.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
            (self.gl.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
        }
    }

    fn draw(
        &self,
        index: usize,
        frame: &DecodedVideoFrame,
        sample: &PlaneSample,
        copy_rgba: bool,
    ) -> Result<()> {
        let (framebuffer, luma_texture, chroma_texture) = {
            let slot = self.slots[index].as_ref().expect("slot created");
            (slot.framebuffer, slot.luma, slot.chroma)
        };
        let matrix = match frame.format.color_matrix {
            ColorMatrix::Bt601 => 0,
            ColorMatrix::Bt709 => 1,
            ColorMatrix::Bt2020 => 2,
        };
        self.gl.clear_error();
        unsafe {
            (self.gl.bind_framebuffer)(GL_FRAMEBUFFER, framebuffer);
            (self.gl.viewport)(0, 0, frame.format.width as i32, frame.format.height as i32);
            (self.gl.use_program)(self.program.name);
            (self.gl.active_texture)(GL_TEXTURE0);
            (self.gl.bind_texture)(GL_TEXTURE_2D, luma_texture);
            (self.gl.uniform_1i)(self.program.luma, 0);
            (self.gl.active_texture)(GL_TEXTURE0 + 1);
            (self.gl.bind_texture)(GL_TEXTURE_2D, chroma_texture);
            (self.gl.uniform_1i)(self.program.chroma, 1);
            (self.gl.uniform_2f)(self.program.texture_scale, 1.0, sample.scale_y);
            if let Some(uniform_1ui) = self.gles3.then_some(self.gl.uniform_1ui).flatten() {
                uniform_1ui(self.program.color_matrix, matrix);
                uniform_1ui(
                    self.program.full_range,
                    u32::from(frame.format.color_range == ColorRange::Full),
                );
                uniform_1ui(self.program.sample_bits, sample.sample_bits);
                uniform_1ui(self.program.copy_rgba, u32::from(copy_rgba));
            } else {
                (self.gl.bind_buffer)(GL_ARRAY_BUFFER, self.program.vertex_buffer);
                (self.gl.enable_attrib)(0);
                (self.gl.attrib_pointer)(0, 2, GL_FLOAT, 0, 16, std::ptr::null());
                (self.gl.enable_attrib)(1);
                (self.gl.attrib_pointer)(
                    1,
                    2,
                    GL_FLOAT,
                    0,
                    16,
                    std::ptr::without_provenance(8),
                );
                (self.gl.uniform_1f)(self.program.color_matrix, matrix as f32);
                (self.gl.uniform_1f)(
                    self.program.full_range,
                    if frame.format.color_range == ColorRange::Full {
                        1.0
                    } else {
                        0.0
                    },
                );
                (self.gl.uniform_1f)(self.program.sample_bits, sample.sample_bits as f32);
                (self.gl.uniform_1f)(
                    self.program.copy_rgba,
                    if copy_rgba { 1.0 } else { 0.0 },
                );
            }
            (self.gl.uniform_1f)(self.program.chroma_offset_x, sample.chroma_offset_x);
            (self.gl.uniform_1f)(
                self.program.chroma_bytes,
                if sample.byte_chroma { 1.0 } else { 0.0 },
            );
            (self.gl.uniform_1f)(self.program.luma_width, sample.luma_width);
            (self.gl.uniform_1f)(self.program.chroma_pitch, sample.chroma_pitch);
            (self.gl.draw_arrays)(GL_TRIANGLES, 0, 3);
            (self.gl.bind_framebuffer)(GL_FRAMEBUFFER, 0);
            (self.gl.active_texture)(GL_TEXTURE0);
            (self.gl.use_program)(0);
        }
        self.gl.check("draw the OpenGL ES video frame")
    }

    fn finish_gpu(&self) {
        if let Some(finish) = self.gl.finish {
            unsafe { finish() };
        }
    }

    fn destroy_one_image(&self, image: *mut c_void) {
        if image.is_null() {
            return;
        }
        let Some(destroy) = self.gl.destroy_image else {
            return;
        };
        let display = unsafe { (self.gl.get_current_display)() };
        if display.is_null() {
            return;
        }
        unsafe { destroy(display, image) };
    }

    fn destroy_images_at(&mut self, index: usize) {
        let (luma_image, chroma_image) = {
            let Some(slot) = self.slots[index].as_mut() else {
                return;
            };
            let images = (slot.luma_image, slot.chroma_image);
            slot.luma_image = 0;
            slot.chroma_image = 0;
            images
        };
        if luma_image == 0 && chroma_image == 0 {
            return;
        }
        self.finish_gpu();
        let Some(destroy) = self.gl.destroy_image else {
            return;
        };
        let display = unsafe { (self.gl.get_current_display)() };
        if display.is_null() {
            return;
        }
        for image in [luma_image, chroma_image] {
            if image != 0 {
                unsafe { destroy(display, image as *mut c_void) };
            }
        }
    }

    fn destroy_images(&self, slot: &mut Slot) {
        let pending = slot.luma_image != 0 || slot.chroma_image != 0;
        if pending {
            self.finish_gpu();
        }
        let Some(destroy) = self.gl.destroy_image else {
            slot.luma_image = 0;
            slot.chroma_image = 0;
            return;
        };
        let display = unsafe { (self.gl.get_current_display)() };
        if !display.is_null() {
            for image in [slot.luma_image, slot.chroma_image] {
                if image != 0 {
                    unsafe { destroy(display, image as *mut c_void) };
                }
            }
        }
        slot.luma_image = 0;
        slot.chroma_image = 0;
    }

    fn destroy_slot(&self, mut slot: Slot) {
        self.destroy_images(&mut slot);
        if !self.gl.context_is_current() {
            return;
        }
        unsafe {
            if slot.output != 0 {
                (self.gl.delete_textures)(1, &slot.output);
            }
            if slot.luma != 0 {
                (self.gl.delete_textures)(1, &slot.luma);
            }
            if slot.chroma != 0 {
                (self.gl.delete_textures)(1, &slot.chroma);
            }
            if slot.framebuffer != 0 {
                (self.gl.delete_framebuffers)(1, &slot.framebuffer);
            }
        }
    }

    fn destroy(&mut self) {
        if self.destroyed {
            return;
        }
        self.destroyed = true;
        if self.gl.context_is_current() {
            if self.program.vertex_buffer != 0 {
                unsafe { (self.gl.delete_buffers)(1, &self.program.vertex_buffer) };
            }
            unsafe { (self.gl.delete_program)(self.program.name) };
        }
        let slots: Vec<_> = self.slots.drain(..).flatten().collect();
        for slot in slots {
            self.destroy_slot(slot);
        }
    }
}

impl Drop for GlesPresenter {
    fn drop(&mut self) {
        self.destroy();
    }
}

fn output_format(
    frame: &DecodedVideoFrame,
    color_space: LinuxTextureColorSpace,
) -> Result<GpuTextureFormat> {
    match frame.format.pixel_format {
        PixelFormat::Rgba8 | PixelFormat::Bgra8 | PixelFormat::I420 => {
            if color_space != LinuxTextureColorSpace::Sdr709 {
                return Err(Error::InvalidFormat(
                    "packed or I420 HDR cannot be presented on OpenGL ES".to_owned(),
                ));
            }
            Ok(GpuTextureFormat::Rgba8)
        }
        _ => GpuTextureFormat::for_stream_format(frame.format),
    }
}

fn conversion_inputs(frame: &DecodedVideoFrame, coded_height: u32) -> PlaneSample {
    let scale_y = frame.format.height as f32 / coded_height as f32;
    let sample_bits = if frame.format.pixel_format == PixelFormat::P010 {
        10
    } else {
        8
    };
    let chroma_offset_x = if frame.format.chroma_location == crate::ChromaLocation::Left
        && !frame.format.pixel_format.is_444()
    {
        0.5 / frame.format.width as f32
    } else {
        0.0
    };
    PlaneSample {
        scale_y,
        sample_bits,
        chroma_offset_x,
        byte_chroma: false,
        luma_width: frame.format.width as f32,
        chroma_pitch: 1.0,
    }
}

fn yuv_planes(frame: &DmaBufFrame, pixel_format: PixelFormat) -> Result<(DmaBufPlane, DmaBufPlane)> {
    if !matches!(pixel_format, PixelFormat::Nv12 | PixelFormat::P010) {
        return Err(Error::unavailable(
            Subsystem::OpenGlEs,
            "DMA-BUF import supports NV12 and P010",
        ));
    }
    let p010 = pixel_format == PixelFormat::P010;
    let (luma, chroma) = if frame.layers.len() == 1
        && frame.layers[0].format == if p010 { DRM_FORMAT_P010 } else { DRM_FORMAT_NV12 }
        && frame.layers[0].planes.len() >= 2
    {
        (frame.layers[0].planes[0], frame.layers[0].planes[1])
    } else if frame.layers.len() >= 2
        && frame.layers[0].format == if p010 { DRM_FORMAT_R16 } else { DRM_FORMAT_R8 }
        && frame.layers[1].format == if p010 { DRM_FORMAT_GR1616 } else { DRM_FORMAT_GR88 }
        && !frame.layers[0].planes.is_empty()
        && !frame.layers[1].planes.is_empty()
    {
        (frame.layers[0].planes[0], frame.layers[1].planes[0])
    } else {
        return Err(Error::unavailable(
            Subsystem::OpenGlEs,
            "unsupported DMA-BUF NV12/P010 plane layout",
        ));
    };
    Ok((luma, chroma))
}

fn modifier_is_linear(modifier: u64) -> bool {
    modifier == DRM_FORMAT_MOD_LINEAR || modifier == DRM_FORMAT_MOD_INVALID
}

fn map_linear_nv12(frame: &DecodedVideoFrame) -> Result<(FramePlane, FramePlane)> {
    if frame.format.pixel_format != PixelFormat::Nv12 {
        return Err(Error::unavailable(
            Subsystem::OpenGlEs,
            "OpenGL ES 2 samples linear NV12 only",
        ));
    }
    let dmabuf = frame.dmabuf.as_ref().expect("dma-buf frame");
    if dmabuf
        .objects
        .iter()
        .any(|object| !modifier_is_linear(object.format_modifier))
    {
        return Err(Error::unavailable(
            Subsystem::OpenGlEs,
            "tiled DMA-BUF needs OpenGL ES 3",
        ));
    }
    let (luma, chroma) = yuv_planes(dmabuf, PixelFormat::Nv12)?;
    let height = frame.format.height as usize;
    let width = frame.format.width as usize;
    let luma_rows = copy_dmabuf_rows(dmabuf, &luma, width, height.max(1))?;
    let chroma_rows = copy_dmabuf_rows(dmabuf, &chroma, width, (height / 2).max(1))?;
    Ok((
        FramePlane {
            data: std::sync::Arc::<[u8]>::from(luma_rows),
            stride: width,
            rows: height.max(1),
        },
        FramePlane {
            data: std::sync::Arc::<[u8]>::from(chroma_rows),
            stride: width,
            rows: (height / 2).max(1),
        },
    ))
}

fn copy_dmabuf_rows(
    frame: &DmaBufFrame,
    plane: &DmaBufPlane,
    row_bytes: usize,
    rows: usize,
) -> Result<Vec<u8>> {
    let object = frame.objects.get(plane.object_index).ok_or_else(|| {
        Error::InvalidFormat(format!(
            "DMA-BUF plane object {} is missing",
            plane.object_index
        ))
    })?;
    if object.fd < 0 || row_bytes == 0 || rows == 0 || plane.pitch < row_bytes {
        return Err(Error::InvalidFormat(
            "DMA-BUF plane is too small to copy".to_owned(),
        ));
    }
    let last = plane
        .offset
        .checked_add(
            plane
                .pitch
                .checked_mul(rows - 1)
                .ok_or_else(|| Error::InvalidFormat("DMA-BUF plane size overflow".to_owned()))?,
        )
        .and_then(|start| start.checked_add(row_bytes))
        .ok_or_else(|| Error::InvalidFormat("DMA-BUF plane size overflow".to_owned()))?;
    if last > object.size || object.size == 0 {
        return Err(Error::InvalidFormat(
            "DMA-BUF plane extends past its object".to_owned(),
        ));
    }
    dma_buf_sync(object.fd, DMA_BUF_SYNC_READ | DMA_BUF_SYNC_START);
    let mapped = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            object.size,
            libc::PROT_READ,
            libc::MAP_SHARED,
            object.fd,
            0,
        )
    };
    if mapped == libc::MAP_FAILED {
        dma_buf_sync(object.fd, DMA_BUF_SYNC_READ | DMA_BUF_SYNC_END);
        return Err(Error::unavailable(
            Subsystem::OpenGlEs,
            "the DMA-BUF could not be mapped for OpenGL ES 2",
        ));
    }
    let mapped = MappedDmaBuf {
        ptr: mapped,
        len: object.size,
        fd: object.fd,
    };
    let bytes = unsafe { std::slice::from_raw_parts(mapped.ptr.cast::<u8>(), mapped.len) };
    let mut copied = Vec::with_capacity(row_bytes * rows);
    for row in 0..rows {
        let start = plane.offset + row * plane.pitch;
        copied.extend_from_slice(&bytes[start..start + row_bytes]);
    }
    Ok(copied)
}

struct MappedDmaBuf {
    ptr: *mut c_void,
    len: usize,
    fd: i32,
}

impl Drop for MappedDmaBuf {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.ptr, self.len) };
        dma_buf_sync(self.fd, DMA_BUF_SYNC_READ | DMA_BUF_SYNC_END);
    }
}

const DMA_BUF_SYNC_READ: u64 = 1;
const DMA_BUF_SYNC_START: u64 = 0;
const DMA_BUF_SYNC_END: u64 = 1 << 2;
const DMA_BUF_IOCTL_SYNC: libc::c_ulong = 0x4008_6200;

fn dma_buf_sync(fd: i32, flags: u64) {
    #[repr(C)]
    struct DmaBufSync {
        flags: u64,
    }
    let mut sync = DmaBufSync { flags };
    unsafe {
        libc::ioctl(fd, DMA_BUF_IOCTL_SYNC, &mut sync);
    }
}

fn pack_rows(data: &[u8], stride: usize, row_bytes: usize, rows: usize) -> Result<Vec<u8>> {
    if row_bytes == 0 || stride < row_bytes {
        return Err(Error::InvalidFormat(
            "plane stride is shorter than the visible row".to_owned(),
        ));
    }
    let mut packed = Vec::with_capacity(row_bytes.saturating_mul(rows));
    for row in 0..rows {
        let start = row
            .checked_mul(stride)
            .ok_or_else(|| Error::InvalidFormat("plane size overflow".to_owned()))?;
        let end = start
            .checked_add(row_bytes)
            .ok_or_else(|| Error::InvalidFormat("plane size overflow".to_owned()))?;
        if end > data.len() {
            return Err(Error::InvalidFormat(
                "plane does not contain the visible rows".to_owned(),
            ));
        }
        packed.extend_from_slice(&data[start..end]);
    }
    Ok(packed)
}

fn interleave_i420(
    u_plane: &FramePlane,
    v_plane: &FramePlane,
    width: usize,
    height: usize,
    dest: &mut Vec<u8>,
) {
    let rows = height / 2;
    let pairs = width / 2;
    dest.resize(pairs.saturating_mul(2).saturating_mul(rows), 0);
    for row in 0..rows {
        for col in 0..pairs {
            let out = (row * pairs + col) * 2;
            dest[out] = u_plane.data[row * u_plane.stride + col];
            dest[out + 1] = v_plane.data[row * v_plane.stride + col];
        }
    }
}

fn swizzle_bgra(plane: &FramePlane, width: usize, height: usize, dest: &mut Vec<u8>) {
    dest.resize(width.saturating_mul(height).saturating_mul(4), 0);
    for row in 0..height {
        for col in 0..width {
            let source = row * plane.stride + col * 4;
            let out = (row * width + col) * 4;
            dest[out] = plane.data[source + 2];
            dest[out + 1] = plane.data[source + 1];
            dest[out + 2] = plane.data[source];
            dest[out + 3] = plane.data[source + 3];
        }
    }
}

fn link_program(gl: &Gl, gles3: bool) -> Result<Program> {
    let vertex = compile_shader(
        gl,
        GL_VERTEX_SHADER,
        if gles3 { VERTEX_SHADER } else { ES2_VERTEX_SHADER },
    )?;
    let fragment = compile_shader(
        gl,
        GL_FRAGMENT_SHADER,
        if gles3 {
            FRAGMENT_SHADER
        } else {
            ES2_FRAGMENT_SHADER
        },
    )?;
    let name = unsafe { (gl.create_program)() };
    unsafe {
        (gl.attach_shader)(name, vertex);
        (gl.attach_shader)(name, fragment);
        if !gles3 {
            (gl.bind_attrib)(name, 0, b"position\0".as_ptr().cast());
            (gl.bind_attrib)(name, 1, b"texture_coordinates\0".as_ptr().cast());
        }
        (gl.link_program)(name);
        (gl.delete_shader)(vertex);
        (gl.delete_shader)(fragment);
    }
    let mut linked = 0;
    unsafe { (gl.get_programiv)(name, GL_LINK_STATUS, &mut linked) };
    if linked != 1 {
        let log = program_log(gl, name);
        unsafe { (gl.delete_program)(name) };
        return Err(Error::backend(
            Subsystem::OpenGlEs,
            format!("video shader link failed: {log}"),
        ));
    }
    let location = |uniform: &[u8]| unsafe { (gl.get_uniform)(name, uniform.as_ptr().cast()) };
    let vertex_buffer = if gles3 {
        0
    } else {
        let mut buffer = 0;
        unsafe {
            (gl.gen_buffers)(1, &mut buffer);
            (gl.bind_buffer)(GL_ARRAY_BUFFER, buffer);
            (gl.buffer_data)(
                GL_ARRAY_BUFFER,
                std::mem::size_of_val(&ES2_VERTICES) as isize,
                ES2_VERTICES.as_ptr().cast(),
                GL_STATIC_DRAW,
            );
            (gl.bind_buffer)(GL_ARRAY_BUFFER, 0);
        }
        if gl.check("create the OpenGL ES 2 vertex buffer").is_err() || buffer == 0 {
            unsafe { (gl.delete_program)(name) };
            if buffer != 0 {
                unsafe { (gl.delete_buffers)(1, &buffer) };
            }
            return Err(Error::unavailable(
                Subsystem::OpenGlEs,
                "OpenGL ES 2 vertex buffer could not be created",
            ));
        }
        buffer
    };
    let program = Program {
        name,
        luma: location(b"luma_texture\0"),
        chroma: location(b"chroma_texture\0"),
        texture_scale: location(b"texture_scale\0"),
        color_matrix: location(b"color_matrix\0"),
        full_range: location(b"full_range\0"),
        sample_bits: location(b"sample_bits\0"),
        chroma_offset_x: location(b"chroma_offset_x\0"),
        chroma_bytes: location(b"chroma_bytes\0"),
        luma_width: location(b"luma_width\0"),
        chroma_pitch: location(b"chroma_pitch\0"),
        copy_rgba: location(b"copy_rgba\0"),
        vertex_buffer,
    };
    if [
        program.luma,
        program.chroma,
        program.texture_scale,
        program.color_matrix,
        program.full_range,
        program.sample_bits,
        program.chroma_offset_x,
        program.chroma_bytes,
        program.luma_width,
        program.chroma_pitch,
        program.copy_rgba,
    ]
    .contains(&-1)
    {
        if vertex_buffer != 0 {
            unsafe { (gl.delete_buffers)(1, &vertex_buffer) };
        }
        unsafe { (gl.delete_program)(name) };
        return Err(Error::backend(
            Subsystem::OpenGlEs,
            "video shader is missing a conversion uniform",
        ));
    }
    Ok(program)
}

fn compile_shader(gl: &Gl, kind: u32, source: &str) -> Result<u32> {
    let shader = unsafe { (gl.create_shader)(kind) };
    let text = source.as_ptr().cast();
    let length = i32::try_from(source.len()).unwrap_or(i32::MAX);
    unsafe { (gl.shader_source)(shader, 1, &text, &length) };
    // glCompileShader was not stored separately; link path needs it.
    compile_only(gl, shader)?;
    let mut compiled = 0;
    unsafe { (gl.get_shaderiv)(shader, GL_COMPILE_STATUS, &mut compiled) };
    if compiled == 1 {
        return Ok(shader);
    }
    let log = shader_log(gl, shader);
    unsafe { (gl.delete_shader)(shader) };
    Err(Error::backend(
        Subsystem::OpenGlEs,
        format!("video shader compile failed: {log}"),
    ))
}

fn compile_only(gl: &Gl, shader: u32) -> Result<()> {
    type Compile = unsafe extern "C" fn(u32);
    let compile: Compile = load_symbol(&gl._gles, b"glCompileShader\0")?;
    unsafe { compile(shader) };
    Ok(())
}

fn shader_log(gl: &Gl, shader: u32) -> String {
    let mut length = 0;
    unsafe { (gl.get_shaderiv)(shader, GL_INFO_LOG_LENGTH, &mut length) };
    if length <= 1 {
        return "no compiler log".to_owned();
    }
    let mut bytes = vec![0u8; length as usize];
    unsafe {
        (gl.get_shader_log)(
            shader,
            length,
            std::ptr::null_mut(),
            bytes.as_mut_ptr().cast(),
        );
    }
    String::from_utf8_lossy(&bytes).trim_end_matches('\0').to_owned()
}

fn program_log(gl: &Gl, program: u32) -> String {
    let mut length = 0;
    unsafe { (gl.get_programiv)(program, GL_INFO_LOG_LENGTH, &mut length) };
    if length <= 1 {
        return "no linker log".to_owned();
    }
    let mut bytes = vec![0u8; length as usize];
    unsafe {
        (gl.get_program_log)(
            program,
            length,
            std::ptr::null_mut(),
            bytes.as_mut_ptr().cast(),
        );
    }
    String::from_utf8_lossy(&bytes).trim_end_matches('\0').to_owned()
}

fn open_library(names: &[&str]) -> Result<Library> {
    let mut last = None;
    for name in names {
        match unsafe { Library::new(name) } {
            Ok(library) => return Ok(library),
            Err(error) => last = Some(error),
        }
    }
    Err(Error::unavailable(
        Subsystem::OpenGlEs,
        format!(
            "could not load {}: {last:?}",
            names.first().copied().unwrap_or("libGLESv2")
        ),
    ))
}

fn load_symbol<T: Copy>(library: &Library, name: &[u8]) -> Result<T> {
    let symbol = unsafe { library.get::<T>(name) }.map_err(|error| {
        Error::unavailable(
            Subsystem::OpenGlEs,
            format!("missing {}: {error}", symbol_name(name)),
        )
    })?;
    Ok(*symbol)
}

fn proc_symbol<T>(get_proc: &EglProc, name: &[u8]) -> Option<T> {
    let pointer = unsafe { get_proc(name.as_ptr().cast()) };
    if pointer.is_null() {
        return None;
    }
    Some(unsafe { std::mem::transmute_copy(&pointer) })
}

fn symbol_name(name: &[u8]) -> String {
    String::from_utf8_lossy(name).trim_end_matches('\0').to_owned()
}
