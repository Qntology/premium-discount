-- 접근성 내비게이션 타임트라이얼 (trial/1). 받는 곳은 www/src/navtrials.ts, 규칙(TRIAL_POLICY)은 cron/src/navtrials.ts,
-- 기기 쪽은 crates/pd-nav trial.rs
--
-- nav_trials: 계정의 콜렉션. 계정 · 앱마다 코스 10개까지, 코스 하나는 체크포인트 10개까지.
--   account 는 HMAC 키라 이메일 · 지갑 주소가 그대로 남지 않는다. app · course · steps 는 해시뿐이고, 시간은 시작부터의 구간 시간(100 ms 단위)뿐이다.
--   새 코스가 11번째면 touched 가 가장 작은(가장 오래 달리지 않은) 코스를 지운다.
--   세션(QR 메일 로그인) 행은 사용자가 지울 때까지, 지갑(익명 계정) 행은 last_day 기준 30일까지 둔다.
-- nav_courses: 계정 없는 코스별 합계 (다른 사용자에게 보이는 타임트라이얼). 코스 id 가 없으면 새로 만들고, 있으면 그 코스를
--   처음 등록하는 사용자의 런만 더한다: runs = 등록한 사용자 수, 결정 수 · 구간 합도 사용자마다 한 번. 최고 기록(record)은
--   그 런들 중 가장 빠른 것. 영원히 둔다.
--   체크포인트 id = hashId(<종류> + <해시> + <제스처>). 코스 id 는 체인: hashId(<앱 id> + <첫 체크포인트 id>), 다음부터
--   hashId(<이전 코스 id> + <체크포인트 id>), 결승의 값. 같은 길은 같은 id 라 두 번 등록되지 않는다.
--   steps 의 체크포인트마다 id 와 그 지점까지의 코스 id(course)가 들어 있다.
--   runs(등록한 사용자 수)가 공개 하한(11) 이상인 코스만 내보낸다.
-- nav_registrations: 사용자마다 코스 등록은 한 번. id = hashId(<계정 키> + <코스 id>) (웹의 itemId = hashId(team + cc + link) 방식).
--   계정 키 · 코스 id 를 모르면 되돌릴 수 없는 해시뿐이다. 콜렉션에서 밀려나거나(11번째 코스) 지운 코스를 다시 달려도 다시 등록하지
--   않도록 콜렉션과 따로 두고, 지우지 않는다
-- nav_counts: 계정 없는 (해시, 날짜, 종류, 규칙) 개수. 판정 cron(PD-Verdict v2 implicit)의 입력이다. 해시는 둘 중 하나다:
--   코스 id(런의 SDS 결정), 체크포인트 id(고스트 따라가기 판정: 따라감 premium · 비켜 감 discount)
-- nav_vectors: (해시, 공간)마다 대표 벡터 하나 (처음 받은 것). float32 리틀 엔디언
CREATE TABLE IF NOT EXISTS nav_trials (
	"account" TEXT NOT NULL,
	"kind" TEXT NOT NULL CHECK ("kind" IN ('session', 'wallet')),
	"app" TEXT NOT NULL,
	"course" TEXT NOT NULL,
	-- [{"kind": "screen" | "region", "hash": "0x…"}, …]
	"steps" TEXT NOT NULL,
	"runs" INTEGER NOT NULL,
	"premium" INTEGER NOT NULL,
	"discount" INTEGER NOT NULL,
	-- 최고 기록 · 마지막 런의 구간 시간 [ms, …]
	"best" TEXT NOT NULL,
	"last" TEXT NOT NULL,
	"last_day" INTEGER NOT NULL,
	"touched" INTEGER NOT NULL,
	PRIMARY KEY ("account", "app", "course")
);
CREATE INDEX IF NOT EXISTS idx_nav_trials_order ON nav_trials ("account", "app", "touched");
CREATE INDEX IF NOT EXISTS idx_nav_trials_expiry ON nav_trials ("kind", "last_day");

CREATE TABLE IF NOT EXISTS nav_courses (
	"course" TEXT PRIMARY KEY,
	"app" TEXT NOT NULL,
	"steps" TEXT NOT NULL,
	"runs" INTEGER NOT NULL,
	"premium" INTEGER NOT NULL,
	"discount" INTEGER NOT NULL,
	-- 최고 기록의 구간 시간, 구간 시간의 합 (평균 = sums / runs)
	"record" TEXT NOT NULL,
	"sums" TEXT NOT NULL,
	"last_day" INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_nav_courses_app ON nav_courses ("app", "runs");

CREATE TABLE IF NOT EXISTS nav_counts (
	"hash" TEXT NOT NULL,
	"day" INTEGER NOT NULL,
	"type" INTEGER NOT NULL CHECK ("type" IN (-1, 1)),
	"recipe" TEXT NOT NULL,
	"n" INTEGER NOT NULL,
	PRIMARY KEY ("hash", "day", "type", "recipe")
);

CREATE TABLE IF NOT EXISTS nav_registrations (
	"id" TEXT PRIMARY KEY,
	-- 처음 등록한 런의 날짜
	"day" INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS nav_vectors (
	"hash" TEXT NOT NULL,
	"space" TEXT NOT NULL,
	"dims" INTEGER NOT NULL,
	"vector" BLOB NOT NULL,
	"day" INTEGER NOT NULL,
	PRIMARY KEY ("hash", "space")
);
