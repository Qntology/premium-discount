//! Live accessibility navigation & privacy bridge connecting `lv-screen`,
//! `premium-discount-window`, and `premium-discount-nav`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use lv_core::siglip2::EpochWatch;
use lv_screen::naflex::{redact_rgb_for_naflex, Preprocess};
use lv_screen::sensor::{extract_rois, LogLumSensor, PurgableSensor};
use lv_screen::ScreenGeometry;
use premium_discount_nav::{
    evaluate_privacy, merge, FrameContext, Numbered, PrivacyProbeInput, PrivacyShield, Rect,
    Region, Role, Source, StickyConfig, StickyNumberer, MERGE_IOU,
};
use premium_discount_verdict::Tier;
use premium_discount_window::identify::VisibleWindowEntry;
use premium_discount_window::surface::{assess_surfaces, PxRect};

#[derive(Debug, Clone)]
pub struct NavFrameOutcome {
    pub shield: PrivacyShield,
    pub record: bool,
    pub numbered: Vec<Numbered>,
    pub masked_patches: Vec<u32>,
    pub event_roi_count: usize,
}

pub struct LiveNavFrameRouter {
    sensor: LogLumSensor,
    numberer: StickyNumberer,
    privacy_epoch: Arc<AtomicU64>,
    width: usize,
    height: usize,
    max_patches: usize,
}

impl LiveNavFrameRouter {
    pub fn new(width: usize, height: usize, threshold: f32, max_patches: usize) -> Self {
        Self {
            sensor: LogLumSensor::new(width.max(1), height.max(1), threshold),
            numberer: StickyNumberer::new(StickyConfig::default()),
            privacy_epoch: Arc::new(AtomicU64::new(0)),
            width: width.max(1),
            height: height.max(1),
            max_patches: max_patches.max(1),
        }
    }

    pub fn privacy_epoch(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.privacy_epoch)
    }

    pub fn is_sensor_primed(&self) -> bool {
        self.sensor.is_primed()
    }

    /// Processes a capture frame with OS window signals and tree/vision candidates:
    /// - Enforces `FullLockdown` immediately (increments privacy epoch, zeroizes `LogLumSensor`, resets tracks, `record = false`).
    /// - Enforces `PartialRedaction` via `lv_screen::naflex::redact_rgb_for_naflex`.
    /// - Merges & numbers surviving candidates while pinning `ctx.priority_anchor` within slots 1..=9.
    pub fn process_frame(
        &mut self,
        bgra: &[u8],
        rgb_mut: &mut [u8],
        windows: &[VisibleWindowEntry],
        capture_rect: PxRect,
        tree_candidates: Vec<Region>,
        vision_candidates: Vec<Region>,
        vision_privacy_hit: bool,
        tiers: &HashMap<String, Tier>,
        ctx: &FrameContext,
    ) -> NavFrameOutcome {
        let surface = assess_surfaces(windows, capture_rect);
        let os_lock_hit = surface.full_lockdown();
        let mut os_rects: Vec<Rect> = surface
            .redact_zones
            .iter()
            .filter_map(|&(x, y, w, h)| Rect::new(x, y, w, h))
            .collect();
        os_rects.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));

        let mut all_candidates = tree_candidates;
        let mut event_roi_count = 0usize;
        let epoch_watch = EpochWatch::capture(&self.privacy_epoch);

        if !os_lock_hit {
            let stats = self.sensor.process_bgra(bgra);
            let screen = ScreenGeometry::new(self.width as u32, self.height as u32);
            let rois = extract_rois(&stats, &screen, 4);
            event_roi_count = rois.len();
            for (idx, roi) in rois.into_iter().enumerate() {
                let x = (roi.bbox.x0 / self.width as f32).clamp(0.0, 1.0);
                let y = (roi.bbox.y0 / self.height as f32).clamp(0.0, 1.0);
                let rw = ((roi.bbox.x1 - roi.bbox.x0) / self.width as f32).clamp(0.0, 1.0 - x);
                let rh = ((roi.bbox.y1 - roi.bbox.y0) / self.height as f32).clamp(0.0, 1.0 - y);
                if let Some(rect) = Rect::new(x, y, rw, rh) {
                    all_candidates.push(Region {
                        anchor: format!("event-roi:{idx}"),
                        role: Role::Other,
                        name: String::new(),
                        rect,
                        source: Source::Event,
                        score: 1.0,
                        changed: true,
                        sensitive: false,
                    });
                }
            }
            if epoch_watch.is_current() {
                all_candidates.extend(vision_candidates);
            }
        }

        let probe = PrivacyProbeInput {
            regions: &all_candidates,
            os_capture_protected: os_lock_hit
                && surface
                    .lockdown()
                    .is_some_and(|h| matches!(h.signal, premium_discount_window::surface::SurfaceSignal::CaptureProtected)),
            os_overlay_payment_detected: os_lock_hit
                && surface
                    .lockdown()
                    .is_some_and(|h| !matches!(h.signal, premium_discount_window::surface::SurfaceSignal::CaptureProtected)),
            os_notification_zones: &os_rects,
            vision_privacy_hit,
            seed_phrase_detected: false,
            auth_or_payment_context: false,
        };
        let shield = evaluate_privacy(&probe);
        let record = shield.record();

        let mut masked_patches = Vec::new();
        let filtered = match &shield {
            PrivacyShield::FullLockdown(_) => {
                self.privacy_epoch.fetch_add(1, Ordering::Release);
                self.sensor.purge_privacy_residue();
                self.numberer.reset();
                event_roi_count = 0;
                shield.redact(all_candidates)
            }
            PrivacyShield::PartialRedaction { zones } => {
                let tuples: Vec<(f32, f32, f32, f32)> =
                    zones.iter().map(|r| (r.x, r.y, r.w, r.h)).collect();
                masked_patches = redact_rgb_for_naflex(
                    rgb_mut,
                    self.width,
                    self.height,
                    &tuples,
                    self.max_patches,
                    &Preprocess::RAON_VE,
                );
                shield.redact(all_candidates)
            }
            PrivacyShield::Clear => all_candidates,
        };

        let merged = merge(filtered, MERGE_IOU);
        let (numbered, _) = self.numberer.update(merged, tiers, MERGE_IOU, ctx);

        NavFrameOutcome {
            shield,
            record,
            numbered,
            masked_patches,
            event_roi_count,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use premium_discount_nav::PrivacyTrigger;
    use premium_discount_window::identify::WindowInfo;
    use premium_discount_window::surface::WDA_EXCLUDEFROMCAPTURE;

    fn region(anchor: &str, role: Role, name: &str, r: (f32, f32, f32, f32), source: Source) -> Region {
        Region {
            anchor: anchor.into(),
            role,
            name: name.into(),
            rect: Rect::new(r.0, r.1, r.2, r.3).unwrap(),
            source,
            score: 1.0,
            changed: false,
            sensitive: false,
        }
    }

    #[test]
    fn live_nav_enforces_lockdown_zeroize_toast_redaction_and_priority_slot() {
        let (w, h) = (128usize, 96usize);
        let mut router = LiveNavFrameRouter::new(w, h, 0.25, 64);
        let bgra = vec![160u8; w * h * 4];
        let mut rgb = vec![160u8; w * h * 3];
        let cap = PxRect::new(0, 0, w as i32, h as i32);

        // Build >9 tree candidates with priority anchor at the end
        let mut tree = Vec::new();
        for i in 0..10 {
            tree.push(region(
                &format!("btn:{i}"),
                Role::Button,
                &format!("item-{i}"),
                (0.05 + (i % 5) as f32 * 0.16, 0.10 + (i / 5) as f32 * 0.25, 0.12, 0.08),
                Source::Tree,
            ));
        }
        let vision = vec![region("vis:1", Role::Button, "", (0.10, 0.70, 0.15, 0.12), Source::Vision)];
        let ctx = FrameContext {
            screen_key: Some("shop/cart".into()),
            now_ms: 1000,
            scrolled: false,
            locked_numbers: 0,
            priority_anchor: Some("btn:9".into()),
        };
        let out = router.process_frame(&bgra, &mut rgb, &[], cap, tree.clone(), vision.clone(), false, &HashMap::new(), &ctx);
        assert!(out.record);
        assert!(router.is_sensor_primed());
        assert!(out.numbered.iter().any(|n| n.region.anchor == "btn:9" && n.number.is_some()));

        // PartialRedaction notification toast masks RGB corner & drops toast overlap
        let toast = VisibleWindowEntry {
            info: WindowInfo {
                hwnd: 1,
                pid: 99999,
                exe_path: None,
                exe_name: Some("ShellExperienceHost.exe".into()),
                title: "New notification".into(),
            },
            rect: PxRect::new(96, 72, 128, 96),
            ex_style: 0,
            display_affinity: 0,
            class_name: "Windows.UI.Core.CoreWindow".into(),
        };
        let toast_out = router.process_frame(&bgra, &mut rgb, &[toast], cap, tree.clone(), vision.clone(), false, &HashMap::new(), &ctx);
        assert!(matches!(toast_out.shield, PrivacyShield::PartialRedaction { .. }));
        assert!(toast_out.record);
        assert!(!toast_out.masked_patches.is_empty());

        // FullLockdown via WDA_EXCLUDEFROMCAPTURE bumps epoch, zeroizes sensor, sets record=false
        let bank = VisibleWindowEntry {
            info: WindowInfo {
                hwnd: 2,
                pid: 99998,
                exe_path: None,
                exe_name: Some("bank.exe".into()),
                title: "MyBank".into(),
            },
            rect: cap,
            ex_style: 0,
            display_affinity: WDA_EXCLUDEFROMCAPTURE,
            class_name: "BankFrame".into(),
        };
        let epoch_before = router.privacy_epoch().load(Ordering::Acquire);
        let lock_out = router.process_frame(&bgra, &mut rgb, &[bank], cap, tree, vision, false, &HashMap::new(), &ctx);
        assert_eq!(lock_out.shield, PrivacyShield::FullLockdown(PrivacyTrigger::DisplayAffinityProtected));
        assert!(!lock_out.record);
        assert!(!router.is_sensor_primed());
        assert!(router.privacy_epoch().load(Ordering::Acquire) > epoch_before);
        assert!(lock_out.numbered.iter().all(|n| n.region.source == Source::Tree));
    }
}
