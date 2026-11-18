-- 웹 행동 분석 수집 (web-event/1). 받는 곳은 www/src/analytic.ts, 심는 스크립트는 public/static/js/tracker.js,
-- 읽고 구조화하는 쪽은 사이트 주인의 앱(app/src/analytic.rs, crates/premium-discount-analytic). tracker-logis-center 를 옮겼다.
--
-- web_events: 방문자의 사건. 사이트(cc = hashId(호스트))마다 쌓고, 확인된 사이트 주인에게만 돌려준다.
--   session = hashId(cc + sid): 탭마다 새로 만드는 무작위 sid 의 해시 (원문은 남기지 않는다. 방문자 계정 · 쿠키 · IP 없음)
--   type: enter · click · hover · change · scroll · submit · back · leave
--   data = gzip(JSON): 쿼리 없는 주소, 속성 없는 PUG(글의 이메일 · 숫자 네 개 이상은 #), 셀렉터용 조상 모양(태그 · id · class · 일부 속성).
--          입력값은 없다
--   created_at = 받은 시각 (ms). updated_at = 0: 원시 사건 (구조화는 기기에서 한다. 앱의 DELTA GUARD 가 이 값을 본다)
-- analytic_sites: 사이트 주인. 지갑 서명으로 등록하고, 서버가 https://<호스트>/ 의
--   <meta name="premium-discount-site" content="<지갑 주소>"> 로 확인한다. 한 사이트에 주인이 여럿일 수 있다
CREATE TABLE IF NOT EXISTS web_events (
	"id" TEXT PRIMARY KEY,
	"cc" TEXT NOT NULL,
	"session" TEXT NOT NULL,
	"type" TEXT NOT NULL CHECK ("type" IN ('enter', 'click', 'hover', 'change', 'scroll', 'submit', 'back', 'leave')),
	"seq" INTEGER NOT NULL DEFAULT 0,
	"data" BLOB NOT NULL,
	"created_at" INTEGER NOT NULL,
	"updated_at" INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS web_events_cc_created ON web_events ("cc", "created_at", "id");

CREATE TABLE IF NOT EXISTS analytic_sites (
	"cc" TEXT NOT NULL,
	"host" TEXT NOT NULL,
	"owner" TEXT NOT NULL,
	"verified_at" INTEGER NOT NULL,
	PRIMARY KEY ("cc", "owner")
);
CREATE INDEX IF NOT EXISTS analytic_sites_owner ON analytic_sites ("owner");
