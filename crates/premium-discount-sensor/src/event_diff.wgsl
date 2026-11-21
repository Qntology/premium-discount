// Raon Zena – virtual event-camera sensor (Stage 1).
//
// One invocation per pixel of the captured (DXGI Desktop Duplication) frame:
//   L = log(Y + eps),  Y = Rec.709 luma of the sRGB-encoded colour (0..1)
//   event if |L - L_prev| >= C
// Events are accumulated per TILE x TILE tile (count, |ΔL| energy, signed polarity), and the
// new log-luminance is written back as the reference for the next frame.
// Matches `LogLumSensor::process_bgra` (lib.rs) bit-for-bit up to f32 rounding of log().
//
// Re-priming: L lies in [ln(1/255), ln(1 + 1/255)] ≈ [-5.54, +0.004], so a zeroed prev_log (a new
// buffer, or one cleared by the privacy purge) would make almost every pixel cross |L - 0| >= C on
// the next frame. The host therefore dispatches that frame once with threshold_milli =
// REPRIME_THRESHOLD: prev_log is written, no tile counts an event (the CPU sensor's first frame).

struct Params {
    width: u32,
    height: u32,
    tiles_x: u32,
    threshold_milli: u32,   // C * 1000; REPRIME_THRESHOLD = write prev_log only, no events
};

@group(0) @binding(0) var frame: texture_2d<f32>;
@group(0) @binding(1) var<storage, read_write> prev_log: array<f32>;
@group(0) @binding(2) var<storage, read_write> tile_count: array<atomic<u32>>;
@group(0) @binding(3) var<storage, read_write> tile_energy_milli: array<atomic<u32>>;
@group(0) @binding(4) var<storage, read_write> tile_on: array<atomic<u32>>;
@group(0) @binding(5) var<uniform> params: Params;

const TILE: u32 = 32u;
const EPS: f32 = 1.0 / 255.0;
// Sentinel threshold (`REPRIME_THRESHOLD_MILLI` on the host): re-prime the reference, count nothing.
const REPRIME_THRESHOLD: u32 = 0xFFFFFFFFu;

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) {
        return;
    }
    let c = textureLoad(frame, vec2<i32>(i32(gid.x), i32(gid.y)), 0);
    let y = 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b;
    let l = log(y + EPS);
    let idx = gid.y * params.width + gid.x;
    let d = l - prev_log[idx];
    prev_log[idx] = l;
    let thr = f32(params.threshold_milli) / 1000.0;
    if (params.threshold_milli != REPRIME_THRESHOLD && abs(d) >= thr) {
        let t = (gid.y / TILE) * params.tiles_x + (gid.x / TILE);
        atomicAdd(&tile_count[t], 1u);
        atomicAdd(&tile_energy_milli[t], u32(min(abs(d), 16.0) * 1000.0));
        if (d > 0.0) {
            atomicAdd(&tile_on[t], 1u);
        }
    }
}
