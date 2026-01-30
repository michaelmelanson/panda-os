# Add virtio-gpu 3D support for GPU-accelerated composition

## Problem

The compositor (`panda-kernel/src/compositor/mod.rs`) performs CPU-side pixel-by-pixel alpha blending to composite windows onto the framebuffer. For each dirty region, it iterates every pixel, reads source and destination, blends, and writes back. This is the hot path at ~60fps.

The virtio-gpu device supports a 3D mode (virgl) that can offload this work to the host GPU. The driver (`vendor/virtio-drivers/src/device/gpu.rs`) defines the `VIRGL` feature flag but does not implement any 3D commands.

## Goal

Add enough virtio-gpu 3D support to render textured quads with alpha blending on the GPU. The compositor uploads each window's pixel buffer as a texture, submits draw commands to composite them onto a render target, and flushes the result to the display. CPU composition becomes the fallback path when the host doesn't advertise `VIRTIO_GPU_F_VIRGL`.

## Background

### virtio-gpu 3D architecture

When `VIRTIO_GPU_F_VIRGL` is negotiated, the device accepts commands in the `0x02xx` range. These manage **virgl rendering contexts** and **3D resources**, and allow submitting opaque **virgl command buffers** via `SUBMIT_3D`.

The command buffer contains a packed `u32` stream of virgl sub-commands (defined by virglrenderer, not the virtio spec). Each sub-command has a 1-DWORD header:

```
bits 31..16: payload length (DWORDs, excluding header)
bits 15..8:  object type (for CREATE/BIND/DESTROY_OBJECT)
bits  7..0:  command ID (VIRGL_CCMD_*)
```

Sub-commands set GPU pipeline state (blend, rasterizer, shaders, textures) and issue draws. Shaders use TGSI text format.

### Relevant virtio-gpu commands

| Command | Code | Struct |
|---|---|---|
| `CTX_CREATE` | `0x0200` | `{ hdr, nlen, context_init, debug_name[64] }` |
| `CTX_DESTROY` | `0x0201` | `{ hdr }` |
| `CTX_ATTACH_RESOURCE` | `0x0202` | `{ hdr, resource_id, padding }` |
| `CTX_DETACH_RESOURCE` | `0x0203` | `{ hdr, resource_id, padding }` |
| `RESOURCE_CREATE_3D` | `0x0204` | `{ hdr, resource_id, target, format, bind, width, height, depth, array_size, last_level, nr_samples, flags, padding }` |
| `TRANSFER_TO_HOST_3D` | `0x0205` | `{ hdr, box, offset, resource_id, level, stride, layer_stride }` |
| `TRANSFER_FROM_HOST_3D` | `0x0206` | (same layout) |
| `SUBMIT_3D` | `0x0207` | `{ hdr, size }` followed by `size` bytes of command data |

### Relevant virgl sub-commands

For composition (textured quads with alpha blending), the needed sub-commands are:

- `CREATE_OBJECT` (1) — blend, rasterizer, DSA, shader, vertex elements, sampler state, surface, sampler view
- `BIND_OBJECT` (2) / `BIND_SHADER` — activate state objects
- `SET_FRAMEBUFFER_STATE` (5) — set render target
- `SET_VIEWPORT_STATE` (4) — set viewport transform
- `SET_VERTEX_BUFFERS` (6) — bind vertex buffer
- `SET_SAMPLER_VIEWS` (10) — bind texture
- `BIND_SAMPLER_STATES` (18) — bind sampler
- `SET_CONSTANT_BUFFER` (12) — upload per-draw transform
- `RESOURCE_INLINE_WRITE` (9) — upload vertex data into a buffer resource
- `CLEAR` (7) — clear framebuffer
- `DRAW_VBO` (8) — draw call

## Design

The work is split into three layers, each building on the previous.

### Phase 1: virtio-gpu 3D transport (`vendor/virtio-drivers/src/device/gpu.rs`)

Add protocol definitions and driver methods for the 3D command set.

#### 1.1 Protocol structs

Add `#[repr(C)]` structs for all 3D commands listed above. These go alongside the existing 2D structs in `gpu.rs`. All derive `IntoBytes`, `FromBytes`, `Immutable`, `KnownLayout` for zerocopy serialization.

`ResourceCreate3D` is the most important new struct:

```rust
#[repr(C)]
#[derive(Clone, Copy, IntoBytes, FromBytes, Immutable, KnownLayout)]
struct ResourceCreate3D {
    header: CtrlHeader,
    resource_id: u32,
    target: u32,       // PipeTarget enum
    format: u32,       // PipeFormat enum
    bind: u32,         // PipeBind bitflags
    width: u32,
    height: u32,
    depth: u32,
    array_size: u32,
    last_level: u32,
    nr_samples: u32,
    flags: u32,
    padding: u32,
}
```

Add enums/constants for `PipeTarget` (`BUFFER=0`, `TEXTURE_2D=2`), `PipeFormat` (`B8G8R8A8_UNORM`, `R8_UNORM`, etc.), and `PipeBind` bitflags (`RENDER_TARGET=4`, `SAMPLER_VIEW=2`, `VERTEX_BUFFER=16`).

#### 1.2 Command constants

Add command type constants to the existing `Command` definitions:

```rust
const CTX_CREATE: Command = Command(0x0200);
const CTX_DESTROY: Command = Command(0x0201);
const CTX_ATTACH_RESOURCE: Command = Command(0x0202);
const CTX_DETACH_RESOURCE: Command = Command(0x0203);
const RESOURCE_CREATE_3D: Command = Command(0x0204);
const TRANSFER_TO_HOST_3D: Command = Command(0x0205);
const TRANSFER_FROM_HOST_3D: Command = Command(0x0206);
const SUBMIT_3D: Command = Command(0x0207);
```

#### 1.3 Feature negotiation

Add `Features::VIRGL` to `SUPPORTED_FEATURES`. Store whether the feature was actually negotiated in a field on `VirtIOGpu` so callers can check 3D availability at runtime:

```rust
pub fn supports_virgl(&self) -> bool { self.virgl_enabled }
```

#### 1.4 Driver methods

Add public methods to `VirtIOGpu`:

```rust
pub fn ctx_create(&mut self, ctx_id: u32, debug_name: &str) -> Result
pub fn ctx_destroy(&mut self, ctx_id: u32) -> Result
pub fn ctx_attach_resource(&mut self, ctx_id: u32, resource_id: u32) -> Result
pub fn ctx_detach_resource(&mut self, ctx_id: u32, resource_id: u32) -> Result
pub fn resource_create_3d(&mut self, params: &ResourceCreate3DParams) -> Result
pub fn transfer_to_host_3d(&mut self, params: &Transfer3DParams) -> Result
pub fn transfer_from_host_3d(&mut self, params: &Transfer3DParams) -> Result
pub fn submit_3d(&mut self, ctx_id: u32, cmd_buf: &[u8]) -> Result
pub fn get_capset_info(&mut self, index: u32) -> Result<(u32, u32, u32)>
pub fn get_capset(&mut self, capset_id: u32, capset_version: u32) -> Result<Vec<u8>>
```

**`submit_3d` buffer handling**: The current `request()` method copies the request struct into a fixed `PAGE_SIZE` send buffer, then passes it as a single descriptor. For `SUBMIT_3D`, the header and command data need to be sent together. Two options:

- **Option A**: Concatenate the `CtrlHeader` (with `size` appended) and the command buffer into `queue_buf_send`, as long as `24 + cmd_buf.len() <= PAGE_SIZE`. This works for composition command buffers (a few hundred bytes per frame).
- **Option B**: Use two descriptors in the send chain — one for the header struct, one for the command data. This avoids the copy but requires modifying the virtqueue submission path.

Option A is simpler and sufficient for the composition use case. Add an assertion or error if the command buffer exceeds the send buffer size.

#### 1.5 Capset queries

Implement `GET_CAPSET_INFO` and `GET_CAPSET`. These are needed to confirm the host supports the virgl capset before using 3D commands. The response structs:

```rust
#[repr(C)]
struct RespCapsetInfo {
    header: CtrlHeader,
    capset_id: u32,
    capset_max_version: u32,
    capset_max_size: u32,
    padding: u32,
}

#[repr(C)]
struct RespCapset {
    header: CtrlHeader,
    // followed by variable-length capset data
}
```

For `GET_CAPSET`, the response contains variable-length data. Allocate the receive buffer based on `capset_max_size` from `GET_CAPSET_INFO`. This may require a larger receive buffer or a dedicated DMA allocation for the response.

### Phase 2: virgl command stream builder (`vendor/virtio-drivers/src/device/gpu/virgl.rs`)

New module within `virtio-drivers` providing a typed API for constructing virgl command buffers.

#### 2.1 Command buffer builder

```rust
pub struct VirglCommandBuffer {
    data: Vec<u32>,
}

impl VirglCommandBuffer {
    pub fn new() -> Self
    pub fn as_bytes(&self) -> &[u8]

    // State object creation
    pub fn create_blend(&mut self, handle: u32, params: &BlendParams)
    pub fn create_rasterizer(&mut self, handle: u32, params: &RasterizerParams)
    pub fn create_dsa(&mut self, handle: u32, params: &DsaParams)
    pub fn create_shader(&mut self, handle: u32, shader_type: ShaderType, tgsi: &str)
    pub fn create_vertex_elements(&mut self, handle: u32, elements: &[VertexElement])
    pub fn create_sampler_state(&mut self, handle: u32, params: &SamplerParams)
    pub fn create_surface(&mut self, handle: u32, resource_id: u32, format: u32)
    pub fn create_sampler_view(&mut self, handle: u32, resource_id: u32, format: u32, params: &SamplerViewParams)

    // State binding
    pub fn bind_blend(&mut self, handle: u32)
    pub fn bind_rasterizer(&mut self, handle: u32)
    pub fn bind_dsa(&mut self, handle: u32)
    pub fn bind_shader(&mut self, shader_type: ShaderType, handle: u32)
    pub fn bind_vertex_elements(&mut self, handle: u32)

    // Pipeline configuration
    pub fn set_framebuffer_state(&mut self, surface_handles: &[u32], zsurf_handle: u32)
    pub fn set_viewport_state(&mut self, index: u32, scale: [f32; 3], translate: [f32; 3])
    pub fn set_vertex_buffers(&mut self, buffers: &[VertexBufferBinding])
    pub fn set_sampler_views(&mut self, shader_type: ShaderType, views: &[u32])
    pub fn bind_sampler_states(&mut self, shader_type: ShaderType, handles: &[u32])
    pub fn set_constant_buffer(&mut self, shader_type: ShaderType, data: &[f32])

    // Data upload
    pub fn resource_inline_write(&mut self, resource_id: u32, data: &[u8], stride: u32, layer_stride: u32, box_: &VirglBox)

    // Drawing
    pub fn clear(&mut self, buffers: u32, color: [f32; 4], depth: f64, stencil: u32)
    pub fn draw_vbo(&mut self, params: &DrawParams)

    // Sub-context management
    pub fn create_sub_ctx(&mut self, sub_ctx_id: u32)
    pub fn destroy_sub_ctx(&mut self, sub_ctx_id: u32)
}
```

Each method appends the header DWORD (with length, object type, command ID) followed by the payload DWORDs to the internal `Vec<u32>`.

#### 2.2 Header encoding

```rust
fn encode_header(length: u16, obj_type: u8, cmd: u8) -> u32 {
    ((length as u32) << 16) | ((obj_type as u32) << 8) | (cmd as u32)
}
```

#### 2.3 TGSI shader helpers

Provide the two composition shaders as constants:

```rust
pub const COMPOSITOR_VERTEX_SHADER: &str = "\
VERT\n\
DCL IN[0]\n\
DCL IN[1]\n\
DCL OUT[0], POSITION\n\
DCL OUT[1], GENERIC[0]\n\
DCL CONST[0..3]\n\
DCL TEMP[0]\n\
  0: MUL TEMP[0], CONST[0], IN[0].xxxx\n\
  1: MAD TEMP[0], CONST[1], IN[0].yyyy, TEMP[0]\n\
  2: MAD TEMP[0], CONST[2], IN[0].zzzz, TEMP[0]\n\
  3: ADD OUT[0], TEMP[0], CONST[3]\n\
  4: MOV OUT[1], IN[1]\n\
  5: END\n";

pub const COMPOSITOR_FRAGMENT_SHADER: &str = "\
FRAG\n\
DCL IN[0], GENERIC[0], LINEAR\n\
DCL OUT[0], COLOR\n\
DCL SAMP[0]\n\
DCL SVIEW[0], 2D, FLOAT\n\
  0: TEX OUT[0], IN[0], SAMP[0], 2D\n\
  1: END\n";
```

The vertex shader transforms position by a 4x4 matrix (set via constant buffer per-draw to position/scale each window). The fragment shader samples the window texture.

#### 2.4 Shader encoding

TGSI text is packed into `u32` words. The `create_shader` method handles:
1. Encoding the shader header (type, number of tokens, etc.)
2. Packing the TGSI string bytes into `u32` words (little-endian, NUL-padded)
3. Appending to the command buffer

### Phase 3: compositor integration (`panda-kernel/src/compositor/`, `panda-kernel/src/devices/virtio_gpu/`)

#### 3.1 GPU compositor state

New struct to manage 3D compositor resources:

```rust
// In panda-kernel/src/devices/virtio_gpu/ or compositor/
struct GpuCompositor {
    ctx_id: u32,
    
    // Pipeline state object handles (assigned once during init)
    blend_handle: u32,
    rasterizer_handle: u32,
    dsa_handle: u32,
    vertex_shader_handle: u32,
    fragment_shader_handle: u32,
    vertex_elements_handle: u32,
    sampler_state_handle: u32,
    
    // Render target
    fb_resource_id: u32,
    fb_surface_handle: u32,
    
    // Vertex buffer for quads
    vb_resource_id: u32,
    
    // Per-window texture resources
    window_textures: BTreeMap<u64, WindowTexture>,
    
    // Handle allocator
    next_handle: u32,
    next_resource_id: u32,
}

struct WindowTexture {
    resource_id: u32,
    sampler_view_handle: u32,
    width: u32,
    height: u32,
}
```

#### 3.2 Initialization

On GPU init, after `change_resolution()`:

1. Check `gpu.supports_virgl()`. If false, fall back to CPU composition (existing code).
2. Query capsets to confirm virgl support.
3. `ctx_create(ctx_id=1, "compositor")`.
4. Create the framebuffer as a 3D resource (`PIPE_TEXTURE_2D`, `RENDER_TARGET` bind) instead of 2D. Attach backing, attach to context, set scanout.
5. Create vertex buffer resource (`PIPE_BUFFER`, `VERTEX_BUFFER` bind, size for a few hundred quads).
6. Build initial pipeline state via `VirglCommandBuffer`:
   - Create blend object (enable alpha blending: `SRC_ALPHA` / `ONE_MINUS_SRC_ALPHA`)
   - Create rasterizer (no culling, solid fill)
   - Create DSA (depth test disabled)
   - Create vertex and fragment shaders
   - Create vertex elements (position: float2, texcoord: float2, stride=16)
   - Create sampler state (linear filtering, clamp-to-edge)
   - Create surface for framebuffer resource
7. Submit the command buffer.

#### 3.3 Window texture management

When a window is created:
1. `resource_create_3d(PIPE_TEXTURE_2D, SAMPLER_VIEW, width, height)`
2. `resource_attach_backing` with the window's pixel data DMA buffer
3. `ctx_attach_resource`
4. Submit `create_sampler_view` for the new texture

When a window is resized:
1. Destroy old sampler view and resource
2. Recreate at new size

When a window is destroyed:
1. `ctx_detach_resource`, `resource_detach_backing`, `resource_unref`
2. Remove from `window_textures` map

#### 3.4 Per-frame composition

Replace the inner loop of `WindowManager::composite()`:

```rust
fn composite_gpu(&mut self, gpu_compositor: &mut GpuCompositor) {
    let mut cmds = VirglCommandBuffer::new();

    // Bind all pipeline state
    cmds.bind_blend(gpu_compositor.blend_handle);
    cmds.bind_rasterizer(gpu_compositor.rasterizer_handle);
    cmds.bind_dsa(gpu_compositor.dsa_handle);
    cmds.bind_shader(Vertex, gpu_compositor.vertex_shader_handle);
    cmds.bind_shader(Fragment, gpu_compositor.fragment_shader_handle);
    cmds.bind_vertex_elements(gpu_compositor.vertex_elements_handle);
    cmds.set_framebuffer_state(&[gpu_compositor.fb_surface_handle], 0);
    cmds.set_viewport_state(0, ...);

    // Clear
    cmds.clear(PIPE_CLEAR_COLOR, BACKGROUND_COLOR, 0.0, 0);

    // Upload quad geometry for all visible windows
    let mut vertices: Vec<f32> = Vec::new();
    let mut draw_calls: Vec<(u32, u32)> = Vec::new(); // (start_vertex, sampler_view_handle)

    for window in &self.windows {
        let window = window.lock();
        if !window.visible { continue; }

        let tex = &gpu_compositor.window_textures[&window.id];
        let start = vertices.len() / 4;

        // Two triangles forming a quad (position xy + texcoord st)
        // ... push 6 vertices ...

        draw_calls.push((start as u32, tex.sampler_view_handle));
    }

    // Upload vertex data
    cmds.resource_inline_write(gpu_compositor.vb_resource_id, ...);
    cmds.set_vertex_buffers(&[...]);

    // Issue draw calls
    for (start, sampler_view) in &draw_calls {
        cmds.set_sampler_views(Fragment, &[*sampler_view]);
        cmds.bind_sampler_states(Fragment, &[gpu_compositor.sampler_state_handle]);
        // Set per-window transform via constant buffer
        cmds.set_constant_buffer(Vertex, &transform_matrix);
        cmds.draw_vbo(&DrawParams { start: *start, count: 6, mode: TRIANGLES });
    }

    // Transfer dirty window textures to host
    for window in &self.windows {
        let window = window.lock();
        if !window.dirty { continue; }
        let tex = &gpu_compositor.window_textures[&window.id];
        gpu.transfer_to_host_3d(&Transfer3DParams {
            resource_id: tex.resource_id,
            box_: full_texture_box,
            ..
        });
    }

    // Submit and flush
    gpu.submit_3d(gpu_compositor.ctx_id, cmds.as_bytes());
    gpu.flush();
}
```

#### 3.5 Fallback path

Keep the existing CPU compositor as-is. The compositor checks a flag at startup:

```rust
enum CompositionBackend {
    Cpu,
    Gpu(GpuCompositor),
}
```

The `composite()` method dispatches to `composite_cpu()` (existing code) or `composite_gpu()` based on the active backend.

#### 3.6 Window pixel data as DMA buffers

Currently windows store pixel data as `Vec<u8>`. For GPU composition, window pixel data needs to be in DMA-accessible memory so it can be attached as resource backing. Change `Window::pixel_data` from `Vec<u8>` to a DMA buffer wrapper, or allocate a separate DMA buffer and copy on dirty.

The simpler approach: keep `Vec<u8>` for the CPU path. For the GPU path, use `TRANSFER_TO_HOST_3D` which reads from the resource's attached backing store. The backing store is a DMA allocation that the kernel copies window pixel data into before transfer. This avoids changing the window API.

## Risks and open questions

1. **QEMU configuration**: The host must run QEMU with virglrenderer support (`-device virtio-gpu-pci,virgl=on` or `virtio-vga-gl`). Without this, `VIRTIO_GPU_F_VIRGL` won't be advertised and the driver falls back to CPU composition.

2. **Queue size**: The control virtqueue is currently size 2. A `SUBMIT_3D` command with its response uses one descriptor pair. This should be fine for synchronous submission but leaves no room for pipelining. Consider increasing to 4 or 8 if needed.

3. **Send buffer size**: The `queue_buf_send` is `PAGE_SIZE` (4096 bytes). A composition frame's command buffer should fit well within this for a reasonable number of windows. If it doesn't, split into multiple `SUBMIT_3D` calls.

4. **Virgl command encoding correctness**: The virgl command stream format is effectively documented only by the virglrenderer source code (`vrend_decode.c`). Each sub-command's exact payload layout needs to match what virglrenderer expects. Testing against QEMU is the only reliable validation. Key reference files:
   - `virglrenderer/src/vrend_decode.c` — command decoding
   - `virglrenderer/src/vrend_renderer.c` — command execution
   - `mesa/src/gallium/drivers/virgl/virgl_encode.c` — Mesa's encoder (reference for what to send)

5. **TGSI shader format**: TGSI is a text-based IR. The exact encoding in the `create_shader` command (header fields, string packing) must match virglrenderer's parser. Reference: `vrend_decode.c` `vrend_decode_create_shader()`.

6. **Full-frame vs. dirty-region composition**: The initial GPU path should composite the full frame each time (clear + draw all windows). Dirty-region optimization on the GPU (scissor rects, partial clears) can be added later. The GPU is fast enough that full-frame redraw at 1080p is likely cheaper than the CPU dirty-region path.

7. **Synchronization**: The initial implementation can remain synchronous (submit and wait). Async composition using fences (`GPU_FLAG_FENCE` + `fence_id` in `CtrlHeader`) could be added later to overlap CPU work with GPU rendering.
