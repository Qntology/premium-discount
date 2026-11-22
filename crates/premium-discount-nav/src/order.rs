//! 번호 매기기
//!
//! [`number`] 는 한 프레임만 보고 순서를 처음부터 다시 매긴다(상태 없음). 실시간 오버레이처럼 같은 화면을 계속 다시 매길 때는
//! [`StickyNumberer`] 를 쓴다: 한 번 받은 번호는 그 영역이 화면에 남아 있는 동안 등급 · 위치가 바뀌어도 그대로다.

use std::cmp::Ordering;
use std::collections::HashMap;

use premium_discount_sds::matrix::{exclusive_assign_by_score, ABSENT};
use premium_discount_verdict::Tier;
use serde::{Deserialize, Serialize};

use crate::region::{merge_absorbing, sensitive_screen, Rect, Region, Role, Source, CONTAIN_ABSORB};

/// 번호를 받는 영역 수 (키보드 1 ~ 9). 나머지는 번호 없이 목록 뒤에 남고, 목록 끝은 늘 "직접 입력"이다
pub const MAX_NUMBERED: usize = 9;

/// 읽는 순서의 줄 높이 (창 높이의 5 %): 중심이 같은 줄에 있으면 왼쪽부터 읽는다
const ROW: f32 = 0.05;

/// 무점멸 번호의 줄 묶기: 줄의 평균 중심과 이만큼 안이거나, 바로 앞 영역과 세로로 이 몫 이상 겹치면 같은 줄이다
const ROW_GAP: f32 = 0.045;
const ROW_OVERLAP: f32 = 0.45;
/// 비전 · 이벤트 후보를 앞 프레임의 트랙에 잇는 바닥: IoU 가 이만큼 되거나 중심 거리가 이만큼 안이어야 한다
const TRACK_IOU: f32 = 0.45;
const TRACK_CENTER: f32 = 0.06;
/// 트랙을 이을 때 역할이 같으면 더하는 점수
const TRACK_ROLE_BONUS: f32 = 0.25;
/// 사각형 중심이 이만큼 넘게 움직였으면 평균 내지 않고 바로 새 자리로 옮긴다 (레이아웃 이동)
const SNAP_DISTANCE: f32 = 0.12;
/// 홀로 나온 이벤트 후보는 이만큼 잇달아 보이거나 같은 자리에 이만큼 머물러야 번호 후보가 된다 (한 프레임짜리 이펙트 거르기)
const EVENT_CONFIRM_SCANS: u32 = 2;
const EVENT_CONFIRM_MS: i64 = 250;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Numbered {
    /// 1 ~ 9. 앞의 9개 밖이면 None
    pub number: Option<u8>,
    /// 이 영역의 판정 등급 (판정이 없으면 None)
    pub tier: Option<Tier>,
    pub region: Region,
    /// 무점멸 번호([`StickyNumberer`])에서 번호를 어떻게 받았나. [`number`] 의 결과와 번호가 없는 영역은 None
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<SlotOrigin>,
}

/// 무점멸 번호를 어떻게 받았나
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotOrigin {
    /// 화면에 들어왔거나 빈 번호를 새로 받았다
    Fresh,
    /// 앞 프레임의 번호를 그대로 지켰다
    Retained,
    /// 번호가 다 찼을 때 꼭 넣을 영역(`priority_anchor`)이라 다른 영역의 번호를 넘겨받았다
    Promoted,
}

/// 무점멸 번호의 설정
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StickyConfig {
    /// 정지 화면에서 잠깐 안 보인 영역의 번호를 비워 두고 기다리는 시간
    pub occlusion_grace_ms: i64,
    /// 앞 프레임의 번호 중 이 몫도 남지 않으면 다른 화면으로 보고 처음부터 다시 매긴다
    pub min_survival_ratio: f32,
    /// 비전 · 이벤트 사각형을 따라가는 지수 이동 평균에서 새 사각형의 몫
    pub rect_ema_alpha: f32,
}

impl Default for StickyConfig {
    fn default() -> Self {
        StickyConfig {
            occlusion_grace_ms: 600,
            min_survival_ratio: 0.30,
            rect_ema_alpha: 0.35,
        }
    }
}

/// 한 번 스캔한 화면의 문맥 ([`StickyNumberer::update`]). 모든 칸이 빠져도 되는 JSON 으로 받을 수 있다
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FrameContext {
    /// 화면 키 (`screen_hash` · `page_hash`). 바뀌면 번호를 처음부터 다시 매긴다. None 이면 앞 화면으로 본다
    pub screen_key: Option<String>,
    /// 지금 시각 (기기 시계, ms). 안 보이는 영역의 번호를 비워 두는 시간을 잰다
    pub now_ms: i64,
    /// 직전 스캔 뒤에 스크롤했다: 화면 밖으로 나간 영역의 번호를 기다리지 않고 바로 거둔다
    pub scrolled: bool,
    /// 캐릭터가 말하고 있거나 방금 말한 번호의 비트 (번호 n 은 비트 n, 1 ~ 9). 잠긴 번호는 거두지도 내주지도 않는다
    pub locked_numbers: u16,
    /// 번호 1 ~ 9 안에 꼭 넣을 영역의 위치 단서 (예: 타임트라이얼 고스트의 다음 체크포인트)
    pub priority_anchor: Option<String>,
}

impl FrameContext {
    /// 번호 `number`(1 ~ 9)가 잠겼나 (`locked_numbers` 의 비트 `number`)
    pub fn locked(&self, number: u8) -> bool {
        number < 16 && self.locked_numbers & (1u16 << number) != 0
    }
}

/// 번호 하나를 빌려 쓰는 영역
#[derive(Debug, Clone)]
struct SlotLease {
    /// 영역의 위치 단서 (비전 · 이벤트는 트랙이 물려준 것)
    key: String,
    source: Source,
    last_seen_ms: i64,
}

/// 프레임을 건너 이어 보는 비전 · 이벤트 영역
#[derive(Debug, Clone)]
struct Track {
    /// 처음 본 위치 단서. 같은 영역이면 격자 칸 · 이벤트 순번이 바뀌어도 이 값을 물려준다
    key: String,
    /// 부드럽게 따라간 사각형
    rect: Rect,
    role: Role,
    first_seen_ms: i64,
    last_seen_ms: i64,
    /// 잇달아 본 스캔 수
    hits: u32,
    /// 마지막으로 본 프레임
    frame: u64,
    /// 번호 후보인가 (비전은 바로, 홀로 나온 이벤트는 잇달아 보이거나 머문 뒤)
    confirmed: bool,
}

/// 세션 동안 메모리에만 두는 무점멸 번호 매기기 (디스크에 남기지 않는다). 같은 화면에서 한 번 받은 번호 1 ~ 9 는 그 영역이
/// 화면에 남아 있는 동안 등급 · 위치가 바뀌어도 그대로다. 캐릭터가 "2번 눌러"라고 말하는 동안 2번이 다른 영역으로 가지 않는다.
///
/// 호스트(예: MCP `pd_nav_number`)는 창 · 세션마다 하나를 두고 스캔할 때마다 [`StickyNumberer::update`] 를 부른다. 후보 · `tiers` ·
/// `iou` 는 [`crate::merge`] · [`number`] 에 넘기던 것 그대로이고(사각형은 [`Rect::new`] 로 자른 것), 프레임 문맥([`FrameContext`])에
/// 화면 키 · 지금 시각 · 스크롤 여부 · 말한 번호의 비트 · 꼭 넣을 영역을 담는다. 돌려받는 `bool` 이 참이면 개인정보 화면이라
/// 기록하지 않는다(`record: false`). 창을 바꾸거나 개인정보 잠금에 들어가면 [`StickyNumberer::reset`] 을 부른다.
///
/// 한 번의 [`StickyNumberer::update`]:
/// 1. 화면 키가 바뀌면 처음부터 다시 매긴다.
/// 2. 개인정보 화면이면 비전 · 이벤트 트랙과 그 번호를 바로 버린다(트리 후보만 남는다).
/// 3. 후보를 합친다(포함률 흡수, [`merge_absorbing`]). 비전 · 이벤트 후보는 앞 프레임의 트랙에 이어(IoU 0.45 이상이거나 중심 거리
///    0.06 이하, 점수가 높은 것부터 1:1) 처음 본 위치 단서를 물려받고, 사각형은 지수 이동 평균으로 따라간다(중심이 0.12 넘게
///    움직였거나 스크롤했으면 바로 옮긴다). 홀로 나온 이벤트는 두 번 잇달아 보이거나 250 ms 머물러야 번호 후보가 된다.
/// 4. 앞 프레임의 번호 중 남은 것이 [`StickyConfig::min_survival_ratio`] 에 못 미치면 다른 화면으로 보고 다시 매긴다
///    (남아 있는 잠긴 번호는 지킨다).
/// 5. 남은 영역은 번호를 그대로 둔다. 안 보이는 영역의 번호는 정지 화면이면 [`StickyConfig::occlusion_grace_ms`] 동안 비워 두고
///    기다리고, 스크롤했으면 바로 거둔다. 잠긴 번호는 영역이 안 보여도 거두지 않는다.
/// 6. 빈 번호는 작은 것부터 새 영역에 준다(꼭 넣을 영역 → 등급 → 종류 → 줄 묶음 → 왼쪽부터). 줄은 고정 격자가 아니라 세로 중심을
///    훑어 묶으므로 줄 경계에서 좌표가 조금 흔들려도 순서가 뒤집히지 않는다. 번호가 다 찼는데 꼭 넣을 영역이 번호 밖이면 잠기지
///    않은 번호 하나(안 보이는 영역 → discount → 대화형이 아닌 영역 → 읽는 순서가 늦은 영역)를 넘겨준다.
#[derive(Debug, Clone, Default)]
pub struct StickyNumberer {
    pub cfg: StickyConfig,
    active_screen: Option<String>,
    slots: [Option<SlotLease>; MAX_NUMBERED],
    tracks: Vec<Track>,
    frame: u64,
    serial: u64,
}

impl StickyNumberer {
    pub fn new(cfg: StickyConfig) -> StickyNumberer {
        StickyNumberer { cfg, ..StickyNumberer::default() }
    }

    /// 번호와 트랙을 모두 비운다 (창 전환 · 개인정보 잠금)
    pub fn reset(&mut self) {
        self.active_screen = None;
        self.slots = Default::default();
        self.tracks.clear();
    }

    /// 후보를 합치고 번호를 매긴다. 번호가 있는 영역이 번호 순으로 앞에 오고, 나머지는 읽는 순서로 뒤에 온다.
    /// 비전 · 이벤트 영역의 `anchor` · `rect` 는 트랙이 물려준 위치 단서와 부드럽게 따라간 사각형이다. 둘째 값은 개인정보 화면인가다
    pub fn update(&mut self, candidates: Vec<Region>, tiers: &HashMap<String, Tier>, iou: f32, cx: &FrameContext) -> (Vec<Numbered>, bool) {
        let now = cx.now_ms;
        self.frame += 1;
        if cx.screen_key.is_some() && cx.screen_key != self.active_screen {
            self.reset();
            self.active_screen = cx.screen_key.clone();
        }
        let sensitive = sensitive_screen(&candidates);
        if sensitive {
            self.tracks.clear();
            for slot in &mut self.slots {
                if slot.as_ref().is_some_and(|s| s.source != Source::Tree) {
                    *slot = None;
                }
            }
        }
        let regions = self.follow(merge_absorbing(candidates, iou, CONTAIN_ABSORB), cx);
        let tier_of = |i: usize| tiers.get(&regions[i].anchor).copied();

        // 앞 프레임 번호가 이만큼도 남지 않았으면 다른 화면이다
        let occupied = self.slots.iter().flatten().count();
        let survived = self.slots.iter().flatten().filter(|s| regions.iter().any(|r| r.anchor == s.key)).count();
        let renumber = occupied > 0 && (survived as f32) < self.cfg.min_survival_ratio * occupied as f32;

        let mut numbers: Vec<Option<(u8, SlotOrigin)>> = vec![None; regions.len()];
        for k in 0..MAX_NUMBERED {
            let Some(lease) = self.slots[k].clone() else {
                continue;
            };
            let number = k as u8 + 1;
            let locked = cx.locked(number);
            match (0..regions.len()).find(|&i| numbers[i].is_none() && regions[i].anchor == lease.key) {
                Some(i) if !renumber || locked => {
                    numbers[i] = Some((number, SlotOrigin::Retained));
                    self.slots[k] = Some(SlotLease {
                        key: lease.key,
                        source: regions[i].source,
                        last_seen_ms: now,
                    });
                }
                None if !renumber && (locked || (!cx.scrolled && now - lease.last_seen_ms <= self.cfg.occlusion_grace_ms)) => {}
                _ => self.slots[k] = None,
            }
        }

        let rows = rows(&regions);
        let order = |a: usize, b: usize| {
            let (ra, rb) = (&regions[a], &regions[b]);
            tier_rank(tier_of(a))
                .cmp(&tier_rank(tier_of(b)))
                .then(ra.role.priority().cmp(&rb.role.priority()))
                .then(rows[a].cmp(&rows[b]))
                .then(ra.rect.center().0.total_cmp(&rb.rect.center().0))
                .then(a.cmp(&b))
        };
        let priority = |i: usize| cx.priority_anchor.as_deref() == Some(regions[i].anchor.as_str());
        let mut waiting: Vec<usize> = (0..regions.len()).filter(|&i| numbers[i].is_none()).collect();
        waiting.sort_by(|&a, &b| priority(b).cmp(&priority(a)).then(order(a, b)));
        let free: Vec<usize> = (0..MAX_NUMBERED).filter(|&k| self.slots[k].is_none()).collect();
        for (&i, &k) in waiting.iter().zip(&free) {
            numbers[i] = Some((k as u8 + 1, SlotOrigin::Fresh));
            self.slots[k] = Some(SlotLease {
                key: regions[i].anchor.clone(),
                source: regions[i].source,
                last_seen_ms: now,
            });
        }

        // 번호가 다 찼는데 꼭 넣을 영역이 번호 밖이다: 잠기지 않은 번호 중 가장 내줄 만한 것을 넘겨준다
        if let Some(p) = (0..regions.len()).find(|&i| priority(i) && numbers[i].is_none()) {
            let holder = |k: usize| (0..regions.len()).find(|&i| numbers[i].is_some_and(|(n, _)| usize::from(n) == k + 1));
            let expendable = |k: usize| match holder(k) {
                None => (true, false, false),
                Some(i) => (false, tier_of(i) == Some(Tier::Discount), !interactive(regions[i].role)),
            };
            let victim = (0..MAX_NUMBERED).filter(|&k| self.slots[k].is_some() && !cx.locked(k as u8 + 1)).max_by(|&a, &b| {
                expendable(a)
                    .cmp(&expendable(b))
                    .then_with(|| match (holder(a), holder(b)) {
                        (Some(x), Some(y)) => order(x, y),
                        _ => Ordering::Equal,
                    })
                    .then(a.cmp(&b))
            });
            if let Some(k) = victim {
                if let Some(i) = holder(k) {
                    numbers[i] = None;
                }
                numbers[p] = Some((k as u8 + 1, SlotOrigin::Promoted));
                self.slots[k] = Some(SlotLease {
                    key: regions[p].anchor.clone(),
                    source: regions[p].source,
                    last_seen_ms: now,
                });
            }
        }

        let mut index: Vec<usize> = (0..regions.len()).collect();
        index.sort_by(|&a, &b| match (numbers[a], numbers[b]) {
            (Some((x, _)), Some((y, _))) => x.cmp(&y),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => order(a, b),
        });
        let mut taken: Vec<Option<Region>> = regions.into_iter().map(Some).collect();
        let out = index
            .into_iter()
            .filter_map(|i| {
                let region = taken[i].take()?;
                Some(Numbered {
                    number: numbers[i].map(|(n, _)| n),
                    tier: tiers.get(&region.anchor).copied(),
                    region,
                    origin: numbers[i].map(|(_, o)| o),
                })
            })
            .collect();

        // 이번에 못 본 트랙: 잠긴 번호의 영역이거나, 번호 후보였고 정지 화면에서 기다리는 시간 안이면 남긴다
        let held: Vec<String> = (0..MAX_NUMBERED)
            .filter(|&k| cx.locked(k as u8 + 1))
            .filter_map(|k| self.slots[k].as_ref().map(|s| s.key.clone()))
            .collect();
        let (frame, grace) = (self.frame, self.cfg.occlusion_grace_ms);
        self.tracks
            .retain(|t| t.frame == frame || held.contains(&t.key) || (t.confirmed && !cx.scrolled && now - t.last_seen_ms <= grace));
        (out, sensitive)
    }

    /// 번호가 다 찼을 때 10번 이후 요소를 여는 순환 배지 뱅크 페이징 (Phase 7 Part 1, `#0` 키 / 음성 "다음 번호").
    ///
    /// * `bank == 0` 은 [`StickyNumberer::update`] 와 100% 동일하다.
    /// * `bank >= 1` 은 `cx.locked_numbers` 에 잠긴 발화 번호는 그 자리에 고정 핀으로 보호하고, 남는 번호 칸을
    ///   `bank` 페이지에 해당하는 10번 이후 후보들에게 배정한다.
    /// * 셋째 반환값은 총 페이지 수(`total_banks >= 1`)다.
    pub fn update_bank(&mut self, candidates: Vec<Region>, tiers: &HashMap<String, Tier>, iou: f32, cx: &FrameContext, bank: usize) -> (Vec<Numbered>, bool, usize) {
        let (mut base, sensitive) = self.update(candidates, tiers, iou, cx);
        let total = base.len();
        let total_banks = total.div_ceil(MAX_NUMBERED).max(1);
        let page = bank % total_banks;
        if page == 0 || total <= MAX_NUMBERED {
            return (base, sensitive, total_banks);
        }
        // 잠긴 발화 번호는 페이지 전환 시에도 고정 핀으로 지킨다.
        let mut pinned_slots = [false; MAX_NUMBERED];
        for item in &mut base {
            if let Some(n) = item.number {
                if cx.locked(n) {
                    pinned_slots[usize::from(n - 1)] = true;
                } else {
                    item.number = None;
                    item.origin = None;
                }
            }
        }
        let free_slots: Vec<u8> = (0..MAX_NUMBERED).filter(|&k| !pinned_slots[k]).map(|k| k as u8 + 1).collect();
        let start = page * MAX_NUMBERED;
        let overflow_indices: Vec<usize> = (start..total).filter(|&i| base[i].number.is_none()).collect();
        for (&idx, &num) in overflow_indices.iter().zip(&free_slots) {
            base[idx].number = Some(num);
            base[idx].origin = Some(SlotOrigin::Fresh);
        }
        base.sort_by(|a, b| match (a.number, b.number) {
            (Some(x), Some(y)) => x.cmp(&y),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        });
        (base, sensitive, total_banks)
    }

    /// 비전 · 이벤트 후보를 앞 프레임의 트랙에 잇는다. 이어지면 트랙의 위치 단서를 물려받고 사각형을 부드럽게 따라간다. 홀로 나온
    /// 이벤트 후보는 번호 후보가 될 때까지(두 번 잇달아 보이거나 250 ms 머묾) 내보내지 않는다. 트리 후보는 그대로 지나간다
    fn follow(&mut self, merged: Vec<Region>, cx: &FrameContext) -> Vec<Region> {
        let (now, frame) = (cx.now_ms, self.frame);
        let alpha = self.cfg.rect_ema_alpha.clamp(0.0, 1.0);
        let moving: Vec<usize> = (0..merged.len()).filter(|&i| merged[i].source != Source::Tree).collect();
        // 점수가 높은 짝부터 1:1 로 잇는다 (이을 수 없는 칸은 ABSENT). 이중 센터링은 쓰지 않는다: 이웃 트랙 둘에 걸친 후보가 있으면
        // 다른 후보의 하나뿐인 짝을 빼앗아 번호가 새로 나기 때문이다
        let scores: Vec<Vec<f32>> = moving.iter().map(|&i| self.tracks.iter().map(|t| track_score(&merged[i], t)).collect()).collect();
        let assigned = exclusive_assign_by_score(&scores, 0.0, f32::NEG_INFINITY);
        let mut track_of: Vec<Option<usize>> = vec![None; merged.len()];
        for (row, &i) in moving.iter().enumerate() {
            track_of[i] = assigned.get(row).copied().flatten().map(|(t, _, _)| t);
        }
        let mut taken: Vec<String> = merged.iter().filter(|r| r.source == Source::Tree).map(|r| r.anchor.clone()).collect();
        taken.extend(self.tracks.iter().map(|t| t.key.clone()));
        let mut out = Vec::with_capacity(merged.len());
        for (i, mut region) in merged.into_iter().enumerate() {
            if region.source == Source::Tree {
                out.push(region);
                continue;
            }
            let t = match track_of[i] {
                Some(t) => {
                    let track = &mut self.tracks[t];
                    let snap = cx.scrolled || region.rect.center_distance(&track.rect) > SNAP_DISTANCE;
                    track.rect = if snap { region.rect } else { blend(&track.rect, &region.rect, alpha) };
                    t
                }
                None => {
                    // 새 트랙: 위치 단서가 이미 쓰이고 있으면(다른 영역이 물려받은 칸) 겹치지 않게 번호를 붙인다
                    let key = if taken.contains(&region.anchor) {
                        self.serial += 1;
                        format!("{}~{}", region.anchor, self.serial)
                    } else {
                        region.anchor.clone()
                    };
                    taken.push(key.clone());
                    self.tracks.push(Track {
                        key,
                        rect: region.rect,
                        role: region.role,
                        first_seen_ms: now,
                        last_seen_ms: now,
                        hits: 0,
                        frame,
                        confirmed: false,
                    });
                    self.tracks.len() - 1
                }
            };
            let track = &mut self.tracks[t];
            track.role = region.role;
            track.hits += 1;
            track.last_seen_ms = now;
            track.frame = frame;
            track.confirmed |= region.source != Source::Event || track.hits >= EVENT_CONFIRM_SCANS || now - track.first_seen_ms >= EVENT_CONFIRM_MS;
            if track.confirmed {
                region.anchor = track.key.clone();
                region.rect = track.rect;
                out.push(region);
            }
        }
        out
    }
}

/// 후보와 트랙의 이음 점수: IoU + 중심이 가까운 정도 + 역할이 같으면 덤. IoU 가 [`TRACK_IOU`] 에 못 미치고 중심도
/// [`TRACK_CENTER`] 보다 멀면 이을 수 없다([`ABSENT`])
fn track_score(region: &Region, track: &Track) -> f32 {
    let iou = region.rect.iou(&track.rect);
    let distance = region.rect.center_distance(&track.rect);
    if iou < TRACK_IOU && distance > TRACK_CENTER {
        return ABSENT;
    }
    let near = (1.0 - distance / TRACK_CENTER).max(0.0);
    iou + near + if region.role == track.role { TRACK_ROLE_BONUS } else { 0.0 }
}

/// 지수 이동 평균: 새 사각형을 `alpha` 만큼 섞는다
fn blend(old: &Rect, new: &Rect, alpha: f32) -> Rect {
    let mix = |a: f32, b: f32| a + (b - a) * alpha;
    Rect::new(mix(old.x, new.x), mix(old.y, new.y), mix(old.w, new.w), mix(old.h, new.h)).unwrap_or(*new)
}

fn interactive(role: Role) -> bool {
    matches!(role, Role::Input | Role::Button | Role::Link | Role::Menu)
}

/// 읽는 순서의 줄 묶음 (스윕 라인): 세로 중심 순으로 훑으며 줄의 평균 중심과 [`ROW_GAP`] 안이거나 바로 앞 영역과 세로로
/// [`ROW_OVERLAP`] 이상 겹치면 같은 줄이다. 고정 격자로 자르지 않으므로 줄 경계에서 좌표가 조금 흔들려도 줄이 바뀌지 않는다
fn rows(regions: &[Region]) -> Vec<usize> {
    let mut by_y: Vec<usize> = (0..regions.len()).collect();
    by_y.sort_by(|&a, &b| regions[a].rect.center().1.total_cmp(&regions[b].rect.center().1).then(a.cmp(&b)));
    let mut row_of = vec![0usize; regions.len()];
    let (mut row, mut sum, mut count) = (0usize, 0.0f32, 0usize);
    let mut prev: Option<&Rect> = None;
    for i in by_y {
        let rect = &regions[i].rect;
        let cy = rect.center().1;
        if let Some(p) = prev {
            let overlap = ((rect.y + rect.h).min(p.y + p.h) - rect.y.max(p.y)).max(0.0);
            let shorter = rect.h.min(p.h);
            let same_row = (cy - sum / count as f32).abs() <= ROW_GAP || (shorter > 0.0 && overlap / shorter >= ROW_OVERLAP);
            if !same_row {
                row += 1;
                sum = 0.0;
                count = 0;
            }
        }
        sum += cy;
        count += 1;
        row_of[i] = row;
        prev = Some(rect);
    }
    row_of
}

fn tier_rank(tier: Option<Tier>) -> u8 {
    match tier {
        Some(Tier::Premium) => 0,
        Some(Tier::Discount) => 2,
        _ => 1,
    }
}

fn order(a: &Numbered, b: &Numbered) -> Ordering {
    let ((ax, ay), (bx, by)) = (a.region.rect.center(), b.region.rect.center());
    tier_rank(a.tier)
        .cmp(&tier_rank(b.tier))
        .then(a.region.role.priority().cmp(&b.region.role.priority()))
        .then(((ay / ROW).floor() as i32).cmp(&((by / ROW).floor() as i32)))
        .then(ax.total_cmp(&bx))
}

/// 영역에 번호를 매긴다. `tiers` 는 위치 단서(`anchor`) → 판정 등급이다(로컬 판정, 서버 판정을 받았으면 그것).
/// premium 영역이 앞, discount 영역이 뒤로 가고, 같은 등급 안에서는 종류(입력 → 버튼 → 링크 → 메뉴 → 본문 → 그림 → 기타),
/// 그다음 읽는 순서(위 → 아래, 왼쪽 → 오른쪽)다. 앞의 [`MAX_NUMBERED`] 개가 번호 1 ~ 9 를 받는다.
pub fn number(regions: Vec<Region>, tiers: &HashMap<String, Tier>) -> Vec<Numbered> {
    let mut items: Vec<Numbered> = regions
        .into_iter()
        .map(|region| Numbered {
            number: None,
            tier: tiers.get(&region.anchor).copied(),
            region,
            origin: None,
        })
        .collect();
    items.sort_by(order);
    for (i, item) in items.iter_mut().take(MAX_NUMBERED).enumerate() {
        item.number = Some(i as u8 + 1);
    }
    items
}

/// 2단계 재귀 3×3 격자 타겟팅 (`Recursive 3x3 GridInvoke`, Phase 7 Part 1).
/// `cell` 은 1..=9 (왼쪽 위 1 → 오른쪽 아래 9)이며, 접근성 트리가 없는 캔버스·인게임 지형 조작에 쓰인다.
pub fn grid_subrect(parent: Rect, cell: u8) -> Option<Rect> {
    if !(1..=9).contains(&cell) {
        return None;
    }
    let idx = cell - 1;
    let row = (idx / 3) as f32;
    let col = (idx % 3) as f32;
    let cw = parent.w / 3.0;
    let ch = parent.h / 3.0;
    Rect::new(parent.x + col * cw, parent.y + row * ch, cw, ch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::region;
    use crate::{Role, Source};

    fn anchors(items: &[Numbered]) -> Vec<&str> {
        items.iter().map(|n| n.region.anchor.as_str()).collect()
    }

    #[test]
    fn premium_first_discount_last_then_role_then_reading_order() {
        let regions = vec![
            region("link-b", Role::Link, "도움말", (0.6, 0.02, 0.1, 0.03), Source::Tree),
            region("link-a", Role::Link, "공지", (0.4, 0.02, 0.1, 0.03), Source::Tree),
            region("button", Role::Button, "검색", (0.7, 0.2, 0.1, 0.05), Source::Tree),
            region("input", Role::Input, "검색어", (0.1, 0.2, 0.5, 0.05), Source::Tree),
            region("ad", Role::Image, "", (0.0, 0.5, 0.3, 0.3), Source::Vision),
            region("menu", Role::Menu, "메뉴", (0.0, 0.0, 0.1, 0.05), Source::Tree),
        ];
        let tiers = HashMap::from([("menu".to_string(), Tier::Premium), ("input".to_string(), Tier::Discount)]);
        let out = number(regions, &tiers);
        assert_eq!(anchors(&out), ["menu", "button", "link-a", "link-b", "ad", "input"]);
        assert_eq!(out.iter().map(|n| n.number).collect::<Vec<_>>(), (1..=6).map(Some).collect::<Vec<_>>());
        assert_eq!(out[0].tier, Some(Tier::Premium));
        assert_eq!(out[5].tier, Some(Tier::Discount));
    }

    fn at(screen: &str, now_ms: i64) -> FrameContext {
        FrameContext {
            screen_key: Some(screen.into()),
            now_ms,
            ..FrameContext::default()
        }
    }

    fn number_of(items: &[Numbered], anchor: &str) -> Option<u8> {
        items.iter().find(|n| n.region.anchor == anchor).and_then(|n| n.number)
    }

    fn button(anchor: &str, x: f32, y: f32) -> Region {
        region(anchor, Role::Button, anchor, (x, y, 0.2, 0.04), Source::Tree)
    }

    #[test]
    fn cn04_numbers_hold_through_tier_changes_and_row_jitter() {
        // A · B 는 같은 종류로 한 줄에 있고 중심이 격자 줄 경계(0.05) 둘레에서 ±0.004 흔들린다. C 는 아래의 링크
        let frame = |ya: f32, yb: f32| {
            vec![
                region("a", Role::Button, "확인", (0.1, ya - 0.01, 0.2, 0.02), Source::Tree),
                region("b", Role::Button, "취소", (0.5, yb - 0.01, 0.2, 0.02), Source::Tree),
                region("c", Role::Link, "도움말", (0.1, 0.5, 0.2, 0.05), Source::Tree),
            ]
        };
        let mut sticky = StickyNumberer::new(StickyConfig::default());
        let (out, sensitive) = sticky.update(frame(0.052, 0.048), &HashMap::new(), crate::MERGE_IOU, &at("s", 0));
        assert!(!sensitive);
        assert_eq!(anchors(&out), ["a", "b", "c"], "줄 묶음으로 같은 줄이라 왼쪽(A)부터");
        assert_eq!(
            out.iter().map(|n| (n.number, n.origin)).collect::<Vec<_>>(),
            (1..=3).map(|k| (Some(k), Some(SlotOrigin::Fresh))).collect::<Vec<_>>()
        );
        // 고정 격자로 줄을 자르는 number 는 B 를 윗줄로 보아 A · B 순서가 뒤집힌다
        assert_eq!(anchors(&number(frame(0.052, 0.048), &HashMap::new()))[..2], ["b", "a"]);

        // 다음 스캔: C 가 premium 으로 오르고 A · B 의 중심이 반대로 흔들려도 번호는 그대로, C 의 등급만 바뀐다
        let tiers = HashMap::from([("c".to_string(), Tier::Premium)]);
        let (out, _) = sticky.update(frame(0.048, 0.052), &tiers, crate::MERGE_IOU, &at("s", 100));
        assert_eq!((number_of(&out, "a"), number_of(&out, "b"), number_of(&out, "c")), (Some(1), Some(2), Some(3)));
        assert_eq!(out[2].tier, Some(Tier::Premium));
        assert!(out.iter().all(|n| n.origin == Some(SlotOrigin::Retained)));
        // 상태 없이 다시 매기면 C 가 1번으로 튄다
        assert_eq!(number(frame(0.048, 0.052), &tiers)[0].region.anchor, "c");
    }

    #[test]
    fn cn05_drifting_vision_keeps_its_number_and_one_tick_flashes_get_none() {
        let mut sticky = StickyNumberer::default();
        let tiers = HashMap::new();
        let input = region("form>input#q", Role::Input, "검색어", (0.1, 0.1, 0.5, 0.1), Source::Tree);
        let vision = |anchor: &str, x: f32| region(anchor, Role::Button, "", (x, 0.6, 0.1, 0.05), Source::Vision);
        let event = |anchor: &str, rect: (f32, f32, f32, f32)| region(anchor, Role::Other, "", rect, Source::Event);
        let (out, _) = sticky.update(vec![input.clone(), vision("cell:2,3", 0.40)], &tiers, crate::MERGE_IOU, &at("s", 0));
        assert_eq!((number_of(&out, "form>input#q"), number_of(&out, "cell:2,3")), (Some(1), Some(2)));

        // 비전 버튼이 0.02 옮겨 cell:2,4 가 되고, 빈 배경에 한 프레임짜리 이벤트 두 개, 입력창 안에 작은 변화
        let frame = vec![
            input.clone(),
            vision("cell:2,4", 0.42),
            event("roi:0", (0.85, 0.85, 0.05, 0.05)),
            event("roi:1", (0.2, 0.12, 0.05, 0.04)),
            event("roi:2", (0.05, 0.9, 0.04, 0.04)),
        ];
        let (out, _) = sticky.update(frame, &tiers, crate::MERGE_IOU, &at("s", 100));
        assert_eq!(anchors(&out), ["form>input#q", "cell:2,3"], "이벤트는 아직 번호 후보가 아니다");
        assert_eq!(out[1].number, Some(2), "비전 버튼은 처음 본 위치 단서와 번호를 물려받는다");
        assert!((out[1].region.rect.x - 0.407).abs() < 1e-4, "사각형은 지수 이동 평균으로 따라간다: {:?}", out[1].region.rect);
        assert!(out[0].region.changed, "입력창 안의 변화는 입력창의 바뀜으로 흡수된다");

        // 같은 자리의 이벤트가 다음 스캔에도 보이면(순번은 roi:1 로 바뀜) 번호를 받고, 사라진 한 프레임짜리 이벤트는 끝내 받지 않는다
        let frame = vec![input, vision("cell:2,4", 0.42), event("roi:1", (0.85, 0.85, 0.05, 0.05))];
        let (out, _) = sticky.update(frame, &tiers, crate::MERGE_IOU, &at("s", 200));
        assert_eq!(number_of(&out, "cell:2,3"), Some(2));
        assert_eq!(number_of(&out, "roi:0"), Some(3));
        assert!(out.iter().all(|n| n.region.anchor != "roi:2" && n.region.anchor != "roi:1"));
        assert_eq!(sticky.tracks.len(), 2, "사라진 한 프레임짜리 트랙은 버린다");
    }

    #[test]
    fn cn06_occlusion_waits_600ms_but_scrolling_reclaims_and_sensitive_screens_purge() {
        // 케이스 1: 정지 화면에서 2번이 300 ms 안 보였다가 돌아온다
        let tiers = HashMap::new();
        let (a, b, c) = (button("a", 0.1, 0.1), button("b", 0.1, 0.2), button("c", 0.1, 0.3));
        let mut sticky = StickyNumberer::default();
        sticky.update(vec![a.clone(), b.clone(), c.clone()], &tiers, crate::MERGE_IOU, &at("s", 0));
        let d = button("d", 0.1, 0.4);
        let (out, _) = sticky.update(vec![a.clone(), c.clone(), d.clone()], &tiers, crate::MERGE_IOU, &at("s", 100));
        assert_eq!(number_of(&out, "d"), Some(4), "비워 둔 2번은 새 영역이 가져가지 않는다");
        let (out, _) = sticky.update(vec![a.clone(), c.clone(), d.clone()], &tiers, crate::MERGE_IOU, &at("s", 200));
        assert_eq!(number_of(&out, "d"), Some(4));
        let (out, _) = sticky.update(vec![a.clone(), b.clone(), c.clone(), d.clone()], &tiers, crate::MERGE_IOU, &at("s", 300));
        assert_eq!((number_of(&out, "b"), out[1].origin), (Some(2), Some(SlotOrigin::Retained)), "돌아오면 원래 번호");
        // 600 ms 를 넘겨 안 보이면 번호를 거둔다
        let e = button("e", 0.1, 0.5);
        let (out, _) = sticky.update(vec![a.clone(), c.clone(), d.clone(), e.clone()], &tiers, crate::MERGE_IOU, &at("s", 1_000));
        assert_eq!(number_of(&out, "e"), Some(2));

        // 케이스 2: 스크롤해서 위의 1 · 2번이 화면 밖으로 나가고 아래에 새 버튼이 나온다
        let mut sticky = StickyNumberer::default();
        sticky.update(vec![a.clone(), b.clone(), button("c", 0.1, 0.5), button("d", 0.1, 0.6)], &tiers, crate::MERGE_IOU, &at("s", 0));
        let after = vec![button("c", 0.1, 0.3), button("d", 0.1, 0.4), button("e", 0.1, 0.8), button("f", 0.1, 0.9)];
        let mut still = sticky.clone();
        let scrolled = FrameContext { scrolled: true, ..at("s", 100) };
        let (out, _) = sticky.update(after.clone(), &tiers, crate::MERGE_IOU, &scrolled);
        assert_eq!(anchors(&out), ["e", "f", "c", "d"]);
        assert_eq!(
            out.iter().map(|n| n.number).collect::<Vec<_>>(),
            [Some(1), Some(2), Some(3), Some(4)],
            "1 · 2번을 바로 거둬 새 버튼에 준다"
        );
        // 스크롤하지 않았다면 1 · 2번을 비워 두고 기다린다
        let (out, _) = still.update(after, &tiers, crate::MERGE_IOU, &at("s", 100));
        assert_eq!((number_of(&out, "e"), number_of(&out, "f")), (Some(5), Some(6)));

        // 개인정보 화면에 들어가면 비전 트랙과 그 번호를 바로 버린다
        let mut sticky = StickyNumberer::default();
        let vision = region("cell:5,5", Role::Image, "", (0.5, 0.5, 0.2, 0.2), Source::Vision);
        let (out, _) = sticky.update(vec![a.clone(), vision.clone()], &tiers, crate::MERGE_IOU, &at("s", 0));
        assert_eq!((number_of(&out, "a"), number_of(&out, "cell:5,5"), sticky.tracks.len()), (Some(1), Some(2), 1));
        let mut password = region("form>input#pw", Role::Input, "비밀번호", (0.3, 0.3, 0.4, 0.05), Source::Tree);
        password.sensitive = true;
        let (out, sensitive) = sticky.update(vec![a.clone(), vision, password], &tiers, crate::MERGE_IOU, &at("s", 100));
        assert!(sensitive);
        assert_eq!(sticky.tracks.len(), 0);
        assert_eq!(anchors(&out), ["a", "form>input#pw"]);
        assert_eq!(number_of(&out, "form>input#pw"), Some(2), "비전이 쥐고 있던 2번은 바로 비었다");
    }

    #[test]
    fn cn12_spoken_numbers_stay_put_through_scrolls_and_promotions() {
        let tiers = HashMap::new();
        let (a, b, c) = (button("a", 0.1, 0.1), button("b", 0.1, 0.2), button("c", 0.1, 0.3));
        let mut sticky = StickyNumberer::default();
        sticky.update(vec![a.clone(), b.clone(), c.clone()], &tiers, crate::MERGE_IOU, &at("s", 0));
        // 캐릭터가 "2번" 이라고 말하는 중에 스크롤로 B 가 사라지고 E 가 나온다: 2번은 E 에게 가지 않는다
        let speaking = FrameContext {
            scrolled: true,
            locked_numbers: 1 << 2,
            ..at("s", 100)
        };
        assert!(speaking.locked(2) && !speaking.locked(1));
        let e = button("e", 0.1, 0.4);
        let mut quiet = sticky.clone();
        let (out, _) = sticky.update(vec![a.clone(), c.clone(), e.clone()], &tiers, crate::MERGE_IOU, &speaking);
        assert_eq!(number_of(&out, "e"), Some(4));
        let (out, _) = quiet.update(vec![a, c, e], &tiers, crate::MERGE_IOU, &FrameContext { locked_numbers: 0, ..speaking });
        assert_eq!(number_of(&out, "e"), Some(2));
    }

    #[test]
    fn a_priority_anchor_takes_the_most_expendable_unlocked_number() {
        // 버튼 9개가 1 ~ 9번, 본문 P 는 번호 밖
        let mut regions: Vec<Region> = (0..9).map(|k| button(&format!("r{k}"), 0.1, 0.05 + 0.09 * k as f32)).collect();
        regions.push(region("p", Role::Text, "다음 체크포인트", (0.6, 0.5, 0.3, 0.05), Source::Tree));
        let mut sticky = StickyNumberer::default();
        let (out, _) = sticky.update(regions.clone(), &HashMap::new(), crate::MERGE_IOU, &at("s", 0));
        assert_eq!((number_of(&out, "r4"), number_of(&out, "p")), (Some(5), None));
        let mut locked = sticky.clone();

        // r4 가 discount 가 되고 P 를 꼭 넣어야 한다: r4 의 5번을 넘겨받는다
        let tiers = HashMap::from([("r4".to_string(), Tier::Discount)]);
        let cx = FrameContext {
            priority_anchor: Some("p".into()),
            ..at("s", 100)
        };
        let (out, _) = sticky.update(regions.clone(), &tiers, crate::MERGE_IOU, &cx);
        let p = out.iter().find(|n| n.region.anchor == "p").unwrap();
        assert_eq!((p.number, p.origin), (Some(5), Some(SlotOrigin::Promoted)));
        assert_eq!(number_of(&out, "r4"), None);
        assert_eq!(out.iter().filter(|n| n.number.is_some()).count(), MAX_NUMBERED);
        // 5번이 잠겼으면(말하는 중) 다른 번호 중 읽는 순서가 가장 늦은 9번을 넘겨받는다
        let (out, _) = locked.update(regions, &tiers, crate::MERGE_IOU, &FrameContext { locked_numbers: 1 << 5, ..cx });
        assert_eq!((number_of(&out, "p"), number_of(&out, "r4"), number_of(&out, "r8")), (Some(9), Some(5), None));
    }

    #[test]
    fn a_new_screen_or_a_mostly_new_layout_renumbers_from_scratch() {
        let tiers = HashMap::new();
        let first: Vec<Region> = ["a", "b", "c", "d", "e"].iter().enumerate().map(|(k, n)| button(n, 0.1, 0.1 * (k as f32 + 1.0))).collect();
        let mut sticky = StickyNumberer::default();
        sticky.update(first, &tiers, crate::MERGE_IOU, &at("s", 0));
        // 다섯 중 하나(A)만 남고 아래로 옮겼다: 20 % < 30 % 라 다시 매긴다
        let mut second: Vec<Region> = ["f", "g", "h", "i"].iter().enumerate().map(|(k, n)| button(n, 0.1, 0.1 * (k as f32 + 1.0))).collect();
        second.push(button("a", 0.1, 0.9));
        let (out, _) = sticky.update(second.clone(), &tiers, crate::MERGE_IOU, &at("s", 100));
        assert_eq!(anchors(&out), ["f", "g", "h", "i", "a"]);
        assert!(out.iter().all(|n| n.origin == Some(SlotOrigin::Fresh)));
        // 화면 키가 바뀌면 처음부터: premium 이 앞 번호
        let tiers = HashMap::from([("a".to_string(), Tier::Premium)]);
        let (out, _) = sticky.update(second.clone(), &tiers, crate::MERGE_IOU, &at("t", 200));
        assert_eq!((number_of(&out, "a"), out[0].origin), (Some(1), Some(SlotOrigin::Fresh)));
        // reset 뒤에도 처음부터
        sticky.reset();
        let (out, _) = sticky.update(second, &HashMap::new(), crate::MERGE_IOU, &at("t", 300));
        assert_eq!(number_of(&out, "a"), Some(5));
    }

    #[test]
    fn frame_context_and_origins_are_json_friendly() {
        let cx: FrameContext = serde_json::from_value(serde_json::json!({ "screen_key": "0xabc", "now_ms": 5, "locked_numbers": 4 })).unwrap();
        assert_eq!((cx.screen_key.as_deref(), cx.now_ms, cx.scrolled, cx.priority_anchor.as_deref()), (Some("0xabc"), 5, false, None));
        assert!(cx.locked(2));
        assert_eq!(serde_json::from_value::<FrameContext>(serde_json::json!({})).unwrap(), FrameContext::default());
        assert_eq!(
            serde_json::from_value::<StickyConfig>(serde_json::json!({ "occlusion_grace_ms": 900 })).unwrap().min_survival_ratio,
            0.30
        );
        // 상태 없는 number 의 결과에는 origin 칸이 없다
        let plain = number(vec![button("a", 0.1, 0.1)], &HashMap::new());
        assert!(serde_json::to_value(&plain[0]).unwrap().get("origin").is_none());
        let (sticky, _) = StickyNumberer::default().update(vec![button("a", 0.1, 0.1)], &HashMap::new(), crate::MERGE_IOU, &at("s", 0));
        assert_eq!(serde_json::to_value(&sticky[0]).unwrap()["origin"], "fresh");
    }

    #[test]
    fn only_nine_numbers() {
        let regions: Vec<Region> = (0..12)
            .map(|i| region(&format!("a{i:02}"), Role::Link, "", (0.0, i as f32 * 0.08, 0.1, 0.05), Source::Tree))
            .collect();
        let out = number(regions, &HashMap::new());
        assert_eq!(out.len(), 12);
        assert_eq!(out[8].number, Some(9));
        assert!(out[9..].iter().all(|n| n.number.is_none()));
        assert_eq!(anchors(&out)[..3], ["a00", "a01", "a02"]);
    }

    #[test]
    fn ac02_badge_bank_paging_preserves_spoken_lock_and_reaches_overflow_elements() {
        let make = || {
            (0..15)
                .map(|i| region(&format!("el{i:02}"), Role::Button, &format!("Btn {i}"), (0.05, i as f32 * 0.06, 0.2, 0.04), Source::Tree))
                .collect::<Vec<_>>()
        };
        let mut sticky = StickyNumberer::default();
        let tiers = HashMap::new();
        let cx0 = at("s1", 100);
        let (b0, sens0, total_banks) = sticky.update_bank(make(), &tiers, crate::MERGE_IOU, &cx0, 0);
        assert!(!sens0);
        assert_eq!(total_banks, 2);
        assert_eq!(b0.iter().filter(|n| n.number.is_some()).count(), 9);
        assert_eq!(b0[1].region.anchor, "el01");
        assert_eq!(b0[1].number, Some(2));

        // Lock spoken slot #2 ("el01") and switch to bank 1 (#0 key): #2 stays pinned, el09..el14 get numbers!
        let mut cx1 = at("s1", 200);
        cx1.locked_numbers = 1u16 << 2;
        let (b1, _, banks1) = sticky.update_bank(make(), &tiers, crate::MERGE_IOU, &cx1, 1);
        assert_eq!(banks1, 2);
        let locked_el = b1.iter().find(|n| n.region.anchor == "el01").unwrap();
        assert_eq!(locked_el.number, Some(2), "spoken lock #2 must survive bank switch");
        for idx in 9..15 {
            let key = format!("el{idx:02}");
            let entry = b1.iter().find(|n| n.region.anchor == key).unwrap();
            assert!(entry.number.is_some(), "{key} must be reachable in bank 1");
            assert_ne!(entry.number, Some(2), "overflow item must not steal locked #2");
        }

        // Recursive 3x3 grid zoom: cell 5 (center) then cell 9 (bottom-right).
        let full = Rect::new(0.0, 0.0, 1.0, 1.0).unwrap();
        let mid = grid_subrect(full, 5).unwrap();
        assert!((mid.center().0 - 0.5).abs() < 1e-5 && (mid.center().1 - 0.5).abs() < 1e-5);
        let sub = grid_subrect(mid, 9).unwrap();
        assert!(sub.w < 0.12 && sub.x > 0.5 && sub.y > 0.5);
        assert!(grid_subrect(full, 0).is_none());
    }
}
