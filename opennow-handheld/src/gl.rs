//! OpenGL ES 2 renderer. One shader draws anti-aliased rounded rectangles,
//! outlines, and soft shadows from a signed distance, and samples an RGBA
//! atlas for text and icons. Box art lives in its own textures; quads are
//! batched per texture so a frame is one upload and a few draw calls.

use crate::assets::{self, Image};
use crate::text::{Fonts, Weight};
use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_float, c_int, c_uint, c_void};

const COLOR_BUFFER_BIT: c_uint = 0x4000;
const TRIANGLES: c_uint = 0x0004;
const FLOAT: c_uint = 0x1406;
const UNSIGNED_BYTE: c_uint = 0x1401;
const RGBA: c_uint = 0x1908;
const ARRAY_BUFFER: c_uint = 0x8892;
const STREAM_DRAW: c_uint = 0x88E0;
const VERTEX_SHADER: c_uint = 0x8B31;
const FRAGMENT_SHADER: c_uint = 0x8B30;
const COMPILE_STATUS: c_uint = 0x8B81;
const LINK_STATUS: c_uint = 0x8B82;
const SCISSOR_TEST: c_uint = 0x0C11;
const FRAMEBUFFER: c_uint = 0x8D40;
const BLEND: c_uint = 0x0BE2;
const ONE: c_uint = 1;
const ONE_MINUS_SRC_ALPHA: c_uint = 0x0303;
const TEXTURE_2D: c_uint = 0x0DE1;
const TEXTURE0: c_uint = 0x84C0;
const UNPACK_ALIGNMENT: c_uint = 0x0CF5;
const UNPACK_ROW_LENGTH: c_uint = 0x0CF2;
const TEXTURE_MAG_FILTER: c_uint = 0x2800;
const TEXTURE_MIN_FILTER: c_uint = 0x2801;
const TEXTURE_WRAP_S: c_uint = 0x2802;
const TEXTURE_WRAP_T: c_uint = 0x2803;
const LINEAR: c_int = 0x2601;
const CLAMP_TO_EDGE: c_int = 0x812F;
pub const VENDOR: c_uint = 0x1F00;
pub const RENDERER: c_uint = 0x1F01;
pub const VERSION: c_uint = 0x1F02;

const ATLAS_SIZE: i32 = 1024;

const VERTEX_SHADER_SOURCE: &CStr = c"
attribute vec2 a_position;
attribute vec4 a_color;
attribute vec2 a_uv;
attribute vec2 a_local;
attribute vec4 a_shape;
uniform vec2 u_screen;
varying vec4 v_color;
varying vec2 v_uv;
varying vec2 v_local;
varying vec4 v_shape;
void main() {
    vec2 clip = a_position / u_screen * 2.0 - 1.0;
    gl_Position = vec4(clip.x, -clip.y, 0.0, 1.0);
    v_color = a_color;
    v_uv = a_uv;
    v_local = a_local;
    v_shape = a_shape;
}
";

// v_shape = (half width, half height, corner radius, edge). An edge above zero
// is an outline width, below zero a shadow blur radius. Atlas texels are
// premultiplied; a negative u means the quad is untextured.
const FRAGMENT_SHADER_SOURCE: &CStr = c"
#ifdef GL_ES
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
#endif
uniform sampler2D u_atlas;
varying vec4 v_color;
varying vec2 v_uv;
varying vec2 v_local;
varying vec4 v_shape;
void main() {
    vec4 color = vec4(v_color.rgb * v_color.a, v_color.a);
    if (v_uv.x >= 0.0) {
        color *= texture2D(u_atlas, v_uv);
    }
    if (v_shape.x > 0.0) {
        vec2 q = abs(v_local) - v_shape.xy + v_shape.z;
        float d = length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - v_shape.z;
        float coverage;
        if (v_shape.w < 0.0) {
            coverage = clamp(0.5 - d / -v_shape.w, 0.0, 1.0);
        } else {
            coverage = clamp(0.5 - d, 0.0, 1.0);
            if (v_shape.w > 0.0) {
                coverage *= clamp(0.5 + d + v_shape.w, 0.0, 1.0);
            }
        }
        color *= coverage;
    }
    gl_FragColor = color;
}
";

const FLOATS_PER_VERTEX: usize = 14;

pub struct Gl {
    viewport: unsafe extern "C" fn(c_int, c_int, c_int, c_int),
    clear_color: unsafe extern "C" fn(c_float, c_float, c_float, c_float),
    clear: unsafe extern "C" fn(c_uint),
    get_string: unsafe extern "C" fn(c_uint) -> *const c_char,
    create_shader: unsafe extern "C" fn(c_uint) -> c_uint,
    shader_source: unsafe extern "C" fn(c_uint, c_int, *const *const c_char, *const c_int),
    compile_shader: unsafe extern "C" fn(c_uint),
    get_shader_iv: unsafe extern "C" fn(c_uint, c_uint, *mut c_int),
    get_shader_info_log: unsafe extern "C" fn(c_uint, c_int, *mut c_int, *mut c_char),
    create_program: unsafe extern "C" fn() -> c_uint,
    attach_shader: unsafe extern "C" fn(c_uint, c_uint),
    bind_attrib_location: unsafe extern "C" fn(c_uint, c_uint, *const c_char),
    link_program: unsafe extern "C" fn(c_uint),
    get_program_iv: unsafe extern "C" fn(c_uint, c_uint, *mut c_int),
    get_program_info_log: unsafe extern "C" fn(c_uint, c_int, *mut c_int, *mut c_char),
    use_program: unsafe extern "C" fn(c_uint),
    get_uniform_location: unsafe extern "C" fn(c_uint, *const c_char) -> c_int,
    uniform_2f: unsafe extern "C" fn(c_int, c_float, c_float),
    uniform_1i: unsafe extern "C" fn(c_int, c_int),
    gen_buffers: unsafe extern "C" fn(c_int, *mut c_uint),
    bind_buffer: unsafe extern "C" fn(c_uint, c_uint),
    buffer_data: unsafe extern "C" fn(c_uint, isize, *const c_void, c_uint),
    enable_vertex_attrib_array: unsafe extern "C" fn(c_uint),
    vertex_attrib_pointer: unsafe extern "C" fn(c_uint, c_int, c_uint, u8, c_int, *const c_void),
    draw_arrays: unsafe extern "C" fn(c_uint, c_int, c_int),
    enable: unsafe extern "C" fn(c_uint),
    disable: unsafe extern "C" fn(c_uint),
    scissor: unsafe extern "C" fn(c_int, c_int, c_int, c_int),
    bind_framebuffer: Option<unsafe extern "C" fn(c_uint, c_uint)>,
    blend_func: unsafe extern "C" fn(c_uint, c_uint),
    gen_textures: unsafe extern "C" fn(c_int, *mut c_uint),
    delete_textures: unsafe extern "C" fn(c_int, *const c_uint),
    bind_texture: unsafe extern "C" fn(c_uint, c_uint),
    active_texture: unsafe extern "C" fn(c_uint),
    tex_parameter_i: unsafe extern "C" fn(c_uint, c_uint, c_int),
    tex_image_2d: unsafe extern "C" fn(c_uint, c_int, c_int, c_int, c_int, c_int, c_uint, c_uint, *const c_void),
    tex_sub_image_2d: unsafe extern "C" fn(c_uint, c_int, c_int, c_int, c_int, c_int, c_uint, c_uint, *const c_void),
    pixel_store_i: unsafe extern "C" fn(c_uint, c_int),
}

macro_rules! load {
    ($lookup:expr, $name:literal) => {{
        let pointer: *mut c_void = $lookup(concat!($name, "\0"));
        if pointer.is_null() {
            return Err(format!("OpenGL ES is missing {}", $name));
        }
        unsafe { std::mem::transmute::<*mut c_void, _>(pointer) }
    }};
}

impl Gl {
    pub fn load(lookup: impl Fn(&str) -> *mut c_void) -> Result<Self, String> {
        Ok(Self {
            viewport: load!(lookup, "glViewport"),
            clear_color: load!(lookup, "glClearColor"),
            clear: load!(lookup, "glClear"),
            get_string: load!(lookup, "glGetString"),
            create_shader: load!(lookup, "glCreateShader"),
            shader_source: load!(lookup, "glShaderSource"),
            compile_shader: load!(lookup, "glCompileShader"),
            get_shader_iv: load!(lookup, "glGetShaderiv"),
            get_shader_info_log: load!(lookup, "glGetShaderInfoLog"),
            create_program: load!(lookup, "glCreateProgram"),
            attach_shader: load!(lookup, "glAttachShader"),
            bind_attrib_location: load!(lookup, "glBindAttribLocation"),
            link_program: load!(lookup, "glLinkProgram"),
            get_program_iv: load!(lookup, "glGetProgramiv"),
            get_program_info_log: load!(lookup, "glGetProgramInfoLog"),
            use_program: load!(lookup, "glUseProgram"),
            get_uniform_location: load!(lookup, "glGetUniformLocation"),
            uniform_2f: load!(lookup, "glUniform2f"),
            uniform_1i: load!(lookup, "glUniform1i"),
            gen_buffers: load!(lookup, "glGenBuffers"),
            bind_buffer: load!(lookup, "glBindBuffer"),
            buffer_data: load!(lookup, "glBufferData"),
            enable_vertex_attrib_array: load!(lookup, "glEnableVertexAttribArray"),
            vertex_attrib_pointer: load!(lookup, "glVertexAttribPointer"),
            draw_arrays: load!(lookup, "glDrawArrays"),
            enable: load!(lookup, "glEnable"),
            disable: load!(lookup, "glDisable"),
            scissor: load!(lookup, "glScissor"),
            bind_framebuffer: {
                let pointer = lookup("glBindFramebuffer\0");
                if pointer.is_null() {
                    None
                } else {
                    Some(unsafe { std::mem::transmute(pointer) })
                }
            },
            blend_func: load!(lookup, "glBlendFunc"),
            gen_textures: load!(lookup, "glGenTextures"),
            delete_textures: load!(lookup, "glDeleteTextures"),
            bind_texture: load!(lookup, "glBindTexture"),
            active_texture: load!(lookup, "glActiveTexture"),
            tex_parameter_i: load!(lookup, "glTexParameteri"),
            tex_image_2d: load!(lookup, "glTexImage2D"),
            tex_sub_image_2d: load!(lookup, "glTexSubImage2D"),
            pixel_store_i: load!(lookup, "glPixelStorei"),
        })
    }

    pub fn string(&self, name: c_uint) -> String {
        let pointer = unsafe { (self.get_string)(name) };
        if pointer.is_null() {
            return String::new();
        }
        unsafe { CStr::from_ptr(pointer) }.to_string_lossy().into_owned()
    }

    fn compile(&self, kind: c_uint, source: &CStr) -> Result<c_uint, String> {
        unsafe {
            let shader = (self.create_shader)(kind);
            let pointer = source.as_ptr();
            (self.shader_source)(shader, 1, &pointer, std::ptr::null());
            (self.compile_shader)(shader);
            let mut status = 0;
            (self.get_shader_iv)(shader, COMPILE_STATUS, &mut status);
            if status == 0 {
                let mut log = [0 as c_char; 1024];
                (self.get_shader_info_log)(shader, log.len() as c_int, std::ptr::null_mut(), log.as_mut_ptr());
                return Err(format!("shader compile failed: {}", CStr::from_ptr(log.as_ptr()).to_string_lossy()));
            }
            Ok(shader)
        }
    }
}

pub type Color = [f32; 4];

pub fn with_alpha(color: Color, alpha: f32) -> Color {
    [color[0], color[1], color[2], color[3] * alpha]
}

#[derive(Clone, Copy, Hash, PartialEq, Eq)]
enum Key {
    Glyph { glyph: u16, pixels: u16, weight: Weight },
    Image { image: Image, width: u16, height: u16 },
}

#[derive(Clone, Copy)]
struct Entry {
    uv: [f32; 4],
    width: f32,
    height: f32,
    left: f32,
    top: f32,
    advance: f32,
}

/// Row-based packer for the shared atlas. Running out of room resets the
/// atlas; anything still needed is re-rasterised on its next use.
struct Packer {
    x: i32,
    y: i32,
    row: i32,
}

impl Packer {
    fn allocate(&mut self, width: i32, height: i32) -> Option<(i32, i32)> {
        if width + 1 > ATLAS_SIZE || height + 1 > ATLAS_SIZE {
            return None;
        }
        if self.x + width + 1 > ATLAS_SIZE {
            self.x = 0;
            self.y += self.row;
            self.row = 0;
        }
        if self.y + height + 1 > ATLAS_SIZE {
            return None;
        }
        let place = (self.x, self.y);
        self.x += width + 1;
        self.row = self.row.max(height + 1);
        Some(place)
    }
}

/// A standalone RGBA texture, such as one game's box art. Release it with
/// `Painter::release`.
pub struct Texture {
    id: c_uint,
}

pub struct Painter {
    gl: Gl,
    program: c_uint,
    buffer: c_uint,
    texture: c_uint,
    screen: c_int,
    vertices: Vec<f32>,
    /// (texture, first vertex) for each run of quads sampling one texture.
    batches: Vec<(c_uint, usize)>,
    bound: c_uint,
    packer: Packer,
    cache: HashMap<Key, Entry>,
    fonts: Fonts,
    drawable: (i32, i32),
    scale: f32,
    offset: (f32, f32),
}

impl Painter {
    pub fn new(gl: Gl) -> Result<Self, String> {
        let vertex = gl.compile(VERTEX_SHADER, VERTEX_SHADER_SOURCE)?;
        let fragment = gl.compile(FRAGMENT_SHADER, FRAGMENT_SHADER_SOURCE)?;
        unsafe {
            let program = (gl.create_program)();
            (gl.attach_shader)(program, vertex);
            (gl.attach_shader)(program, fragment);
            for (index, name) in [c"a_position", c"a_color", c"a_uv", c"a_local", c"a_shape"].iter().enumerate() {
                (gl.bind_attrib_location)(program, index as c_uint, name.as_ptr());
            }
            (gl.link_program)(program);
            let mut status = 0;
            (gl.get_program_iv)(program, LINK_STATUS, &mut status);
            if status == 0 {
                let mut log = [0 as c_char; 1024];
                (gl.get_program_info_log)(program, log.len() as c_int, std::ptr::null_mut(), log.as_mut_ptr());
                return Err(format!("program link failed: {}", CStr::from_ptr(log.as_ptr()).to_string_lossy()));
            }
            let mut buffer = 0;
            (gl.gen_buffers)(1, &mut buffer);
            let mut texture = 0;
            (gl.gen_textures)(1, &mut texture);
            (gl.active_texture)(TEXTURE0);
            (gl.bind_texture)(TEXTURE_2D, texture);
            (gl.pixel_store_i)(UNPACK_ALIGNMENT, 1);
            (gl.tex_image_2d)(TEXTURE_2D, 0, RGBA as c_int, ATLAS_SIZE, ATLAS_SIZE, 0, RGBA, UNSIGNED_BYTE, std::ptr::null());
            for (parameter, value) in [
                (TEXTURE_MIN_FILTER, LINEAR),
                (TEXTURE_MAG_FILTER, LINEAR),
                (TEXTURE_WRAP_S, CLAMP_TO_EDGE),
                (TEXTURE_WRAP_T, CLAMP_TO_EDGE),
            ] {
                (gl.tex_parameter_i)(TEXTURE_2D, parameter, value);
            }
            (gl.use_program)(program);
            (gl.uniform_1i)((gl.get_uniform_location)(program, c"u_atlas".as_ptr()), 0);
            let screen = (gl.get_uniform_location)(program, c"u_screen".as_ptr());
            (gl.enable)(BLEND);
            (gl.blend_func)(ONE, ONE_MINUS_SRC_ALPHA);
            Ok(Self {
                gl,
                program,
                buffer,
                texture,
                screen,
                vertices: Vec::with_capacity(128 * 1024),
                batches: Vec::new(),
                bound: texture,
                packer: Packer { x: 0, y: 0, row: 0 },
                cache: HashMap::new(),
                fonts: Fonts::new(),
                drawable: (1, 1),
                scale: 1.0,
                offset: (0.0, 0.0),
            })
        }
    }

    /// Starts a frame. Drawing uses a fixed design canvas that is scaled to
    /// fit the drawable and centred.
    pub fn begin(&mut self, drawable: (i32, i32), canvas: (f32, f32), background: Color) {
        self.drawable = drawable;
        let (width, height) = (drawable.0 as f32, drawable.1 as f32);
        self.scale = (width / canvas.0).min(height / canvas.1);
        self.offset = ((width - canvas.0 * self.scale) / 2.0, (height - canvas.1 * self.scale) / 2.0);
        unsafe {
            (self.gl.viewport)(0, 0, drawable.0, drawable.1);
            (self.gl.clear_color)(background[0], background[1], background[2], 1.0);
            (self.gl.clear)(COLOR_BUFFER_BIT);
            self.default_target();
        }
    }

    /// Back to the window framebuffer after the video presenter borrows GL state.
    fn default_target(&self) {
        unsafe {
            if let Some(bind) = self.gl.bind_framebuffer {
                bind(FRAMEBUFFER, 0);
            }
            (self.gl.viewport)(0, 0, self.drawable.0, self.drawable.1);
            (self.gl.disable)(SCISSOR_TEST);
            (self.gl.enable)(BLEND);
            (self.gl.blend_func)(ONE, ONE_MINUS_SRC_ALPHA);
            // The video upload leaves texture unit 1 active and a non-zero
            // unpack stride. Text then samples the picture instead of the font.
            (self.gl.active_texture)(TEXTURE0);
            (self.gl.pixel_store_i)(UNPACK_ALIGNMENT, 4);
            (self.gl.pixel_store_i)(UNPACK_ROW_LENGTH, 0);
        }
    }

    /// Limits later drawing to a design-space rectangle. Flushes what was
    /// already queued so earlier quads stay unclipped.
    pub fn clip(&mut self, x: f32, y: f32, width: f32, height: f32) {
        self.flush();
        let (left, top) = self.physical(x, y);
        let right = self.offset.0 + (x + width) * self.scale;
        let bottom = self.offset.1 + (y + height) * self.scale;
        let scissor_x = left.floor() as c_int;
        let scissor_y = (self.drawable.1 as f32 - bottom).floor() as c_int;
        let scissor_w = (right.ceil() - left.floor()).max(0.0) as c_int;
        let scissor_h = (bottom.ceil() - top.floor()).max(0.0) as c_int;
        unsafe {
            (self.gl.enable)(SCISSOR_TEST);
            (self.gl.scissor)(scissor_x, scissor_y, scissor_w, scissor_h);
        }
    }

    pub fn clear_clip(&mut self) {
        self.flush();
        unsafe { (self.gl.disable)(SCISSOR_TEST) };
    }

    pub fn end(&mut self) {
        self.flush();
    }

    fn flush(&mut self) {
        if self.vertices.is_empty() {
            return;
        }
        self.default_target();
        let gl = &self.gl;
        unsafe {
            (gl.use_program)(self.program);
            (gl.uniform_2f)(self.screen, self.drawable.0 as f32, self.drawable.1 as f32);
            (gl.bind_buffer)(ARRAY_BUFFER, self.buffer);
            (gl.buffer_data)(
                ARRAY_BUFFER,
                (self.vertices.len() * size_of::<f32>()) as isize,
                self.vertices.as_ptr().cast(),
                STREAM_DRAW,
            );
            let stride = (FLOATS_PER_VERTEX * size_of::<f32>()) as c_int;
            let mut offset = 0;
            for (index, components) in [2, 4, 2, 2, 4].iter().enumerate() {
                (gl.enable_vertex_attrib_array)(index as c_uint);
                (gl.vertex_attrib_pointer)(
                    index as c_uint,
                    *components,
                    FLOAT,
                    0,
                    stride,
                    (offset * size_of::<f32>()) as *const c_void,
                );
                offset += *components as usize;
            }
            let total = self.vertices.len() / FLOATS_PER_VERTEX;
            for (index, &(texture, first)) in self.batches.iter().enumerate() {
                let end = self.batches.get(index + 1).map_or(total, |next| next.1);
                (gl.bind_texture)(TEXTURE_2D, texture);
                (gl.draw_arrays)(TRIANGLES, first as c_int, (end - first) as c_int);
            }
        }
        self.vertices.clear();
        self.batches.clear();
    }

    /// Uploads premultiplied RGBA pixels as a standalone texture.
    pub fn upload(&mut self, width: u32, height: u32, pixels: &[u8]) -> Texture {
        let gl = &self.gl;
        let mut id = 0;
        unsafe {
            (gl.gen_textures)(1, &mut id);
            (gl.bind_texture)(TEXTURE_2D, id);
            (gl.tex_image_2d)(
                TEXTURE_2D, 0, RGBA as c_int, width as c_int, height as c_int, 0, RGBA, UNSIGNED_BYTE, pixels.as_ptr().cast(),
            );
            for (parameter, value) in [
                (TEXTURE_MIN_FILTER, LINEAR),
                (TEXTURE_MAG_FILTER, LINEAR),
                (TEXTURE_WRAP_S, CLAMP_TO_EDGE),
                (TEXTURE_WRAP_T, CLAMP_TO_EDGE),
            ] {
                (gl.tex_parameter_i)(TEXTURE_2D, parameter, value);
            }
        }
        Texture { id }
    }

    /// Frees a texture. Quads already queued this frame are drawn first.
    pub fn release(&mut self, texture: Texture) {
        if self.batches.iter().any(|(id, _)| *id == texture.id) {
            self.flush();
        }
        unsafe { (self.gl.delete_textures)(1, &texture.id) };
    }

    /// Draws a texture the video presenter owns. It is not deleted here.
    pub fn video(&mut self, texture: u32, x: f32, y: f32, width: f32, height: f32) {
        let (left, top) = self.physical(x, y);
        let (width, height) = (width * self.scale, height * self.scale);
        self.bound = texture;
        self.quad(
            left,
            top,
            left + width,
            top + height,
            ([1.0, 1.0, 1.0, 1.0], [1.0, 1.0, 1.0, 1.0]),
            // The framebuffer is OpenGL's bottom-up orientation. The design
            // canvas is top-down, so the picture is sampled from the far edge.
            Some([0.0, 1.0, 1.0, 0.0]),
            (0.0, 0.0),
            [0.0; 4],
        );
        self.bound = self.texture;
    }

    /// Draws a texture filling a rounded rectangle.
    #[allow(clippy::too_many_arguments)]
    pub fn texture(&mut self, texture: &Texture, x: f32, y: f32, width: f32, height: f32, radius: f32, tint: Color) {
        let (left, top) = self.physical(x, y);
        let (width, height) = (width * self.scale, height * self.scale);
        let radius = (radius * self.scale).min(width / 2.0).min(height / 2.0);
        self.bound = texture.id;
        self.quad(
            left,
            top,
            left + width,
            top + height,
            (tint, tint),
            Some([0.0, 0.0, 1.0, 1.0]),
            (left + width / 2.0, top + height / 2.0),
            [width / 2.0, height / 2.0, radius, 0.0],
        );
        self.bound = self.texture;
    }

    fn physical(&self, x: f32, y: f32) -> (f32, f32) {
        (self.offset.0 + x * self.scale, self.offset.1 + y * self.scale)
    }

    /// Emits a quad whose colour runs from `top` to `bottom`.
    #[allow(clippy::too_many_arguments)]
    fn quad(
        &mut self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        (top, bottom): (Color, Color),
        uv: Option<[f32; 4]>,
        center: (f32, f32),
        shape: [f32; 4],
    ) {
        let [u0, v0, u1, v1] = uv.unwrap_or([-1.0, -1.0, -1.0, -1.0]);
        let first = self.vertices.len() / FLOATS_PER_VERTEX;
        if self.batches.last().is_none_or(|(texture, _)| *texture != self.bound) {
            self.batches.push((self.bound, first));
        }
        for (x, y, u, v, color) in [
            (x0, y0, u0, v0, top),
            (x1, y0, u1, v0, top),
            (x0, y1, u0, v1, bottom),
            (x0, y1, u0, v1, bottom),
            (x1, y0, u1, v0, top),
            (x1, y1, u1, v1, bottom),
        ] {
            self.vertices.extend_from_slice(&[
                x, y, color[0], color[1], color[2], color[3], u, v,
                x - center.0, y - center.1, shape[0], shape[1], shape[2], shape[3],
            ]);
        }
    }

    fn shape(&mut self, x: f32, y: f32, width: f32, height: f32, radius: f32, edge: f32, color: Color) {
        let (left, top) = self.physical(x, y);
        let (width, height) = (width * self.scale, height * self.scale);
        let radius = (radius * self.scale).min(width / 2.0).min(height / 2.0);
        let edge = edge * self.scale;
        let pad = 1.0 + if edge < 0.0 { -edge } else { 0.0 };
        let center = (left + width / 2.0, top + height / 2.0);
        self.quad(
            left - pad,
            top - pad,
            left + width + pad,
            top + height + pad,
            (color, color),
            None,
            center,
            [width / 2.0, height / 2.0, radius, edge],
        );
    }

    /// Fills the part of a rounded rectangle below `from_y` with a vertical
    /// gradient, keeping the rectangle's own corner rounding.
    #[allow(clippy::too_many_arguments)]
    pub fn gradient(&mut self, x: f32, y: f32, width: f32, height: f32, radius: f32, from_y: f32, top: Color, bottom: Color) {
        let (left, rect_top) = self.physical(x, y);
        let (_, start) = self.physical(x, from_y);
        let (width, height) = (width * self.scale, height * self.scale);
        let radius = (radius * self.scale).min(width / 2.0).min(height / 2.0);
        let center = (left + width / 2.0, rect_top + height / 2.0);
        self.quad(
            left - 1.0,
            start,
            left + width + 1.0,
            rect_top + height + 1.0,
            (top, bottom),
            None,
            center,
            [width / 2.0, height / 2.0, radius, 0.0],
        );
    }

    pub fn fill(&mut self, x: f32, y: f32, width: f32, height: f32, radius: f32, color: Color) {
        self.shape(x, y, width, height, radius, 0.0, color);
    }

    /// Draws an outline of `thickness` just inside the rectangle's edge.
    pub fn stroke(&mut self, x: f32, y: f32, width: f32, height: f32, radius: f32, thickness: f32, color: Color) {
        self.shape(x, y, width, height, radius, thickness, color);
    }

    pub fn shadow(&mut self, x: f32, y: f32, width: f32, height: f32, radius: f32, blur: f32, color: Color) {
        self.shape(x, y, width, height, radius, -blur.max(0.5), color);
    }

    fn insert(&mut self, width: i32, height: i32, pixels: &[u8]) -> Option<[f32; 4]> {
        let place = match self.packer.allocate(width, height) {
            Some(place) => place,
            None => {
                self.flush();
                self.cache.clear();
                self.packer = Packer { x: 0, y: 0, row: 0 };
                self.packer.allocate(width, height)?
            }
        };
        unsafe {
            (self.gl.active_texture)(TEXTURE0);
            (self.gl.pixel_store_i)(UNPACK_ALIGNMENT, 4);
            (self.gl.pixel_store_i)(UNPACK_ROW_LENGTH, 0);
            (self.gl.bind_texture)(TEXTURE_2D, self.texture);
            (self.gl.tex_sub_image_2d)(
                TEXTURE_2D, 0, place.0, place.1, width, height, RGBA, UNSIGNED_BYTE, pixels.as_ptr().cast(),
            );
        }
        let size = ATLAS_SIZE as f32;
        Some([
            place.0 as f32 / size,
            place.1 as f32 / size,
            (place.0 + width) as f32 / size,
            (place.1 + height) as f32 / size,
        ])
    }

    fn glyph(&mut self, character: char, pixels: u16, weight: Weight) -> Entry {
        let glyph = self.fonts.glyph(character);
        let key = Key::Glyph { glyph, pixels, weight };
        if let Some(entry) = self.cache.get(&key) {
            return *entry;
        }
        let rendered = self.fonts.render(glyph, pixels as f32, weight);
        let mut entry = Entry {
            uv: [-1.0; 4],
            width: rendered.width as f32,
            height: rendered.height as f32,
            left: rendered.left as f32,
            top: rendered.top as f32,
            advance: rendered.advance,
        };
        if rendered.width > 0 && rendered.height > 0 {
            let rgba: Vec<u8> = rendered.coverage.iter().flat_map(|&a| [a, a, a, a]).collect();
            if let Some(uv) = self.insert(rendered.width as i32, rendered.height as i32, &rgba) {
                entry.uv = uv;
            }
        }
        self.cache.insert(key, entry);
        entry
    }

    fn text_pixels(&self, size: f32) -> u16 {
        (size * self.scale).round().clamp(1.0, 400.0) as u16
    }

    /// Width of `text` in design units.
    pub fn measure(&mut self, text: &str, size: f32, weight: Weight) -> f32 {
        let pixels = self.text_pixels(size);
        let total: f32 = text.chars().map(|character| self.glyph(character, pixels, weight).advance).sum();
        total / self.scale
    }

    /// Draws `text` with its line box starting at `y`. Returns the width.
    pub fn text(&mut self, x: f32, y: f32, size: f32, weight: Weight, text: &str, color: Color) -> f32 {
        let pixels = self.text_pixels(size);
        let (left, top) = self.physical(x, y);
        let baseline = (top + self.fonts.ascent(pixels as f32, weight)).round();
        let mut pen = left.round();
        for character in text.chars() {
            let entry = self.glyph(character, pixels, weight);
            if entry.uv[0] >= 0.0 {
                let x0 = pen.round() + entry.left;
                let y0 = baseline - entry.top;
                self.quad(x0, y0, x0 + entry.width, y0 + entry.height, (color, color), Some(entry.uv), (0.0, 0.0), [0.0; 4]);
            }
            pen += entry.advance;
        }
        (pen - left.round()) / self.scale
    }

    /// Draws `text`, shortening it with an ellipsis to fit `max_width`.
    pub fn text_fit(&mut self, x: f32, y: f32, size: f32, weight: Weight, text: &str, max_width: f32, color: Color) {
        if self.measure(text, size, weight) <= max_width {
            self.text(x, y, size, weight, text, color);
            return;
        }
        let mut shortened: String = text.to_owned();
        while !shortened.is_empty() {
            shortened.pop();
            let candidate = format!("{}…", shortened.trim_end());
            if self.measure(&candidate, size, weight) <= max_width {
                self.text(x, y, size, weight, &candidate, color);
                return;
            }
        }
    }

    pub fn image(&mut self, image: Image, x: f32, y: f32, width: f32, height: f32, tint: Color) {
        let (left, top) = self.physical(x, y);
        let (pixel_width, pixel_height) = ((width * self.scale).round() as u16, (height * self.scale).round() as u16);
        if pixel_width == 0 || pixel_height == 0 {
            return;
        }
        let key = Key::Image { image, width: pixel_width, height: pixel_height };
        let uv = match self.cache.get(&key) {
            Some(entry) => entry.uv,
            None => {
                let pixels = assets::rasterize(image, pixel_width as u32, pixel_height as u32);
                let uv = self.insert(pixel_width as i32, pixel_height as i32, &pixels).unwrap_or([-1.0; 4]);
                self.cache.insert(key, Entry { uv, width: 0.0, height: 0.0, left: 0.0, top: 0.0, advance: 0.0 });
                uv
            }
        };
        if uv[0] < 0.0 {
            return;
        }
        let (x0, y0) = (left.round(), top.round());
        self.quad(x0, y0, x0 + pixel_width as f32, y0 + pixel_height as f32, (tint, tint), Some(uv), (0.0, 0.0), [0.0; 4]);
    }
}
