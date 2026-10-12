//! The process-wide shared GPU context (device, queue, vello renderer).
//!
//! Creating a Metal device and compiling vello's compute pipelines takes a few hundred
//! milliseconds, far too long for the host's main thread. [`prewarm`] starts the work on a
//! background thread (call it when a plugin instance is created); views poll
//! [`GpuContext::try_get`] every tick and show the background colour until it is ready.
//! Every view in the process shares the same context.
//!
//! If the device is lost (GPU reset, eGPU unplugged, `MTLDevice` removal) the context marks
//! itself lost; views drop their surfaces and the next `try_get` starts creating a fresh one.

use crate::GfxError;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use vello::wgpu;

/// Number of uncaptured wgpu errors (validation, out-of-memory, internal) since process
/// start. They are logged instead of panicking; tests and the harness assert this stays 0.
static UNCAPTURED_ERRORS: AtomicU64 = AtomicU64::new(0);
/// Generation counter for contexts (1 = first context).
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// Minimum delay before retrying after a failed creation.
const RETRY_DELAY: Duration = Duration::from_secs(2);

/// Total number of uncaptured GPU errors reported by wgpu so far (all contexts).
pub fn gpu_error_count() -> u64 {
    UNCAPTURED_ERRORS.load(Ordering::Relaxed)
}

/// Shared GPU objects. Obtain with [`GpuContext::try_get`] / [`GpuContext::get_blocking`].
pub struct GpuContext {
    /// The wgpu instance (Metal backend on macOS).
    pub instance: wgpu::Instance,
    /// The adapter the device was created from.
    pub adapter: wgpu::Adapter,
    /// The device.
    pub device: wgpu::Device,
    /// The queue.
    pub queue: wgpu::Queue,
    renderer: Mutex<vello::Renderer>,
    blitters: Mutex<Vec<(wgpu::TextureFormat, Arc<wgpu::util::TextureBlitter>)>>,
    lost: Arc<AtomicBool>,
    generation: u64,
    created_in: Duration,
}

impl std::fmt::Debug for GpuContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuContext")
            .field("generation", &self.generation)
            .field("adapter", &self.adapter.get_info().name)
            .field("lost", &self.is_lost())
            .field("created_in", &self.created_in)
            .finish_non_exhaustive()
    }
}

enum Slot {
    Idle,
    Creating,
    Ready(Arc<GpuContext>),
    Failed { error: String, at: Instant },
}

struct Shared {
    slot: Mutex<Slot>,
    changed: Condvar,
}

fn shared() -> &'static Shared {
    static SHARED: Shared = Shared { slot: Mutex::new(Slot::Idle), changed: Condvar::new() };
    &SHARED
}

fn lock_slot() -> MutexGuard<'static, Slot> {
    shared().slot.lock().unwrap_or_else(|e| e.into_inner())
}

/// Starts creating the shared GPU context on a background thread if it does not exist yet
/// (or was lost). Returns immediately; safe to call any number of times from any thread.
pub fn prewarm() {
    let mut slot = lock_slot();
    start_if_needed(&mut slot);
}

/// Decides whether a creation must start; spawns the worker if so.
fn start_if_needed(slot: &mut Slot) {
    let start = match slot {
        Slot::Idle => true,
        Slot::Creating => false,
        Slot::Ready(ctx) => ctx.is_lost(),
        Slot::Failed { at, .. } => at.elapsed() >= RETRY_DELAY,
    };
    if !start {
        return;
    }
    *slot = Slot::Creating;
    let spawned = std::thread::Builder::new().name("pixl-gfx-gpu-init".into()).spawn(|| {
        let result = std::panic::catch_unwind(GpuContext::create)
            .unwrap_or_else(|_| Err(GfxError::Gpu("panic while creating the GPU context".into())));
        let mut slot = lock_slot();
        *slot = match result {
            Ok(ctx) => {
                log::info!(
                    "pixl-gfx: GPU context #{} ready in {:?} on {}",
                    ctx.generation,
                    ctx.created_in,
                    ctx.adapter.get_info().name
                );
                Slot::Ready(Arc::new(ctx))
            }
            Err(e) => {
                log::error!("pixl-gfx: GPU context creation failed: {e}");
                Slot::Failed { error: e.to_string(), at: Instant::now() }
            }
        };
        shared().changed.notify_all();
    });
    if let Err(e) = spawned {
        *slot = Slot::Failed { error: format!("cannot spawn GPU init thread: {e}"), at: Instant::now() };
    }
}

impl GpuContext {
    /// The shared context if it is ready (never blocks). Starts creation in the background
    /// if needed, so polling this from a timer is enough.
    pub fn try_get() -> Option<Arc<GpuContext>> {
        let mut slot = lock_slot();
        if let Slot::Ready(ctx) = &*slot {
            if !ctx.is_lost() {
                return Some(ctx.clone());
            }
        }
        start_if_needed(&mut slot);
        None
    }

    /// The shared context, waiting up to `timeout` for it to be created. For offscreen
    /// rendering and tests — never call this on the main thread of a plugin.
    pub fn get_blocking(timeout: Duration) -> Result<Arc<GpuContext>, GfxError> {
        let deadline = Instant::now() + timeout;
        let mut slot = lock_slot();
        loop {
            match &*slot {
                Slot::Ready(ctx) if !ctx.is_lost() => return Ok(ctx.clone()),
                Slot::Failed { error, at } if at.elapsed() < RETRY_DELAY => {
                    return Err(GfxError::Gpu(error.clone()));
                }
                _ => start_if_needed(&mut slot),
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(GfxError::Gpu("timed out waiting for the GPU context".into()));
            }
            slot = shared().changed.wait_timeout(slot, deadline - now).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    fn create() -> Result<GpuContext, GfxError> {
        let t0 = Instant::now();
        let backends = if cfg!(target_os = "macos") { wgpu::Backends::METAL } else { wgpu::Backends::PRIMARY };
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            display: None,
            backends,
            flags: wgpu::InstanceFlags::from_build_config(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            backend_options: wgpu::BackendOptions::default(),
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            // A plugin UI must not wake a discrete GPU on dual-GPU Macs.
            power_preference: wgpu::PowerPreference::LowPower,
            force_fallback_adapter: false,
            compatible_surface: None,
            ..Default::default()
        }))
        .map_err(|e| GfxError::Gpu(format!("no GPU adapter: {e}")))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("pixl-gfx"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            ..Default::default()
        }))
        .map_err(|e| GfxError::Gpu(format!("cannot create GPU device: {e}")))?;

        let lost = Arc::new(AtomicBool::new(false));
        let generation = GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
        {
            let lost = lost.clone();
            device.set_device_lost_callback(move |reason, message| {
                if !matches!(reason, wgpu::DeviceLostReason::Destroyed) || !message.is_empty() {
                    log::warn!("pixl-gfx: GPU device #{generation} lost ({reason:?}): {message}");
                }
                lost.store(true, Ordering::Release);
            });
        }
        device.on_uncaptured_error(Arc::new(|error: wgpu::Error| {
            UNCAPTURED_ERRORS.fetch_add(1, Ordering::Relaxed);
            log::error!("pixl-gfx: uncaptured GPU error: {error}");
        }));

        let renderer = vello::Renderer::new(
            &device,
            vello::RendererOptions {
                use_cpu: false,
                antialiasing_support: vello::AaSupport::area_only(),
                num_init_threads: std::num::NonZeroUsize::new(1),
                pipeline_cache: None,
            },
        )
        .map_err(|e| GfxError::Gpu(format!("cannot create the vello renderer: {e}")))?;

        Ok(GpuContext {
            instance,
            adapter,
            device,
            queue,
            renderer: Mutex::new(renderer),
            blitters: Mutex::new(Vec::new()),
            lost,
            generation,
            created_in: t0.elapsed(),
        })
    }

    /// Whether the device was lost (the context must be replaced).
    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::Acquire)
    }

    /// Marks the context lost (e.g. after an unrecoverable surface error); the next
    /// [`GpuContext::try_get`] creates a new one.
    pub fn mark_lost(&self) {
        self.lost.store(true, Ordering::Release);
    }

    /// Process-unique number of this context (1 for the first one).
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// How long creating this context took.
    pub fn creation_time(&self) -> Duration {
        self.created_in
    }

    /// Locks the vello renderer (render calls are serialized; uncontended on the main thread).
    pub(crate) fn renderer(&self) -> MutexGuard<'_, vello::Renderer> {
        self.renderer.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A texture blitter writing to `format` (cached per format).
    pub(crate) fn blitter(&self, format: wgpu::TextureFormat) -> Arc<wgpu::util::TextureBlitter> {
        let mut blitters = self.blitters.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, b)) = blitters.iter().find(|(f, _)| *f == format) {
            return b.clone();
        }
        let b = Arc::new(wgpu::util::TextureBlitter::new(&self.device, format));
        blitters.push((format, b.clone()));
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_context_is_reused_and_recovers_from_device_loss() {
        let a = GpuContext::get_blocking(Duration::from_secs(30)).expect("GPU");
        let b = GpuContext::get_blocking(Duration::from_secs(30)).expect("GPU");
        assert!(Arc::ptr_eq(&a, &b), "contexts must be shared");
        assert!(GpuContext::try_get().is_some_and(|c| Arc::ptr_eq(&c, &a)));
        prewarm(); // no-op while ready
        assert!(GpuContext::try_get().is_some_and(|c| Arc::ptr_eq(&c, &a)));

        // Simulate device loss: the next request must build a new context.
        a.mark_lost();
        assert!(a.is_lost());
        let c = GpuContext::get_blocking(Duration::from_secs(30)).expect("GPU after loss");
        assert!(!Arc::ptr_eq(&a, &c));
        assert!(c.generation() > a.generation());
        assert!(!c.is_lost());
    }
}
