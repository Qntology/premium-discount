/*
 * premium.discount 웹 행동 분석 수집 스크립트 (web-event/1). tracker-logis-center 의 tracker.js 를 옮긴 것이다.
 *
 * 사이트 주인이 자기 사이트에 심는다:
 *   <script src="https://premium.discount/static/js/tracker.js" async></script>
 *   (보내는 곳을 바꾸려면 data-endpoint="https://…/analytic/events")
 * 사이트 확인: 첫 화면 <head> 에 <meta name="premium-discount-site" content="<지갑 주소>"> 를 두고, 앱에서 사이트를 등록한다.
 *
 * 무엇을 보내는가: 누름 · 머묾 · 바꿈 · 스크롤 · 제출 · 뒤로 · 떠남 사건과, 그 요소의 "속성 없는 PUG"(태그와 글만) ·
 * 셀렉터용 조상 모양(태그 · id · class · role/name/type/시험용 속성 · 같은 태그 중 순서), 같은 목록에서 고르지 않은 형제(최대 100).
 * 보내지 않는 것: 입력값(비밀번호 칸은 사건도 없음), 링크 · 그림 주소, 쿠키, 쿼리 문자열. 글의 이메일 · 숫자 네 개 이상은 '#'.
 * 방문자를 알아보는 값은 탭마다 새로 만드는 무작위 sid 뿐이다(탭을 닫으면 사라진다). GPC · DNT 를 켠 방문자는 수집하지 않는다.
 * data-pd-ignore 속성을 단 요소 안의 사건은 보내지 않는다.
 *
 * tracker-logis-center 에서 바뀐 것: ethers · marked 묶음(약 530 KB)을 뺐다(해시는 서버가 한다). outerHTML 대신 PUG 를 브라우저에서
 * 만든다(규칙은 contracts/analytic_pug_v1.json, Rust 판은 crates/premium-discount-analytic). 사건을 모아 text/plain 으로 보낸다
 * (사전 요청 없이, 페이지를 떠날 때도 keepalive · sendBeacon 으로).
 */
;(function (root) {
	'use strict'

	var VERSION = 'web-event/1'
	var MAX_RELATED = 100
	var LIST_THRESHOLD = 60
	var TOKEN_BUDGET = 7000
	var SCROLL_STOP_DELAY = 200
	var HOVER_MIN_MS = 500
	var CHANGE_DELAY = 600
	var RAGE_WINDOW_MS = 700
	var RAGE_PRESSES = 3
	var FLUSH_MS = 5000
	var MAX_BATCH = 20
	var LEAVING_BATCH = 4
	var SELECTOR_DEPTH = 5
	var INLINE_MAX_BYTES = 150
	var MAX_DEPTH = 50
	var INDENT = '    '

	var VOID_TAGS = ['area', 'base', 'br', 'col', 'embed', 'hr', 'img', 'input', 'link', 'meta', 'param', 'source', 'track', 'wbr']
	var SKIP_TAGS = ['script', 'style', 'link', 'noscript', 'iframe', 'svg', 'template']
	var PRESERVE_EMPTY = ['td', 'th', 'textarea', 'select', 'button']
	var WRAPPERS = ['div', 'span', 'section', 'article', 'main', 'aside', 'header', 'footer', 'nav', 'p', 'strong', 'b', 'em', 'i', 'center', 'font']
	var MEANINGFUL_ATTRS = ['src', 'href', 'type', 'name', 'value', 'placeholder', 'checked', 'selected', 'disabled', 'readonly', 'rows', 'cols', 'rowspan', 'colspan', 'scope']
	var DATA_BEARING = ['tr', 'td', 'th', 'input', 'select', 'option', 'textarea', 'button', 'img']
	var CHAIN_ATTRS = ['role', 'name', 'type', 'data-testid', 'data-test', 'data-qa', 'data-cy']
	var IGNORED_CLASSES = ['active', 'selected', 'on', 'current', 'focus', 'hover']

	var api = {}

	// NodeList · HTMLCollection 을 보통 배열로 (DOM 구현에 따라 slice 가 NodeList 를 돌려주기도 한다)
	function toArray(list) {
		var out = []
		for (var i = 0; list && i < list.length; i++) out.push(list[i])
		return out
	}

	function tagOf(el) {
		return el && el.tagName ? String(el.tagName).toLowerCase() : ''
	}

	function attr(el, name) {
		return el && el.getAttribute ? el.getAttribute(name) : null
	}

	// ---- 속성 없는 PUG (crates/premium-discount-analytic src/pug.rs 의 html_to_pug 와 같은 규칙) ----

	function isSelectedOption(el) {
		return el.selected === true || (el.hasAttribute && el.hasAttribute('selected'))
	}

	function skipped(el) {
		var tag = tagOf(el)
		if (SKIP_TAGS.indexOf(tag) >= 0) return true
		// 사이트가 수집에서 뺀 영역은 부모의 PUG 에도 넣지 않는다
		if (el.hasAttribute && el.hasAttribute('data-pd-ignore')) return true
		var style = attr(el, 'style')
		if (style) {
			var s = style.toLowerCase()
			if (s.indexOf('position') >= 0 && (s.indexOf('absolute') >= 0 || s.indexOf('fixed') >= 0)) return true
		}
		if (tag === 'img' && (attr(el, 'src') || '').indexOf('base64') >= 0) return true
		return tag === 'option' && !isSelectedOption(el)
	}

	// 자식 노드. 이어진 글 조각은 하나로 합친다(파서 · DOM 조작에 따라 갈려 있어도 같은 PUG 가 되게)
	function children(el) {
		var out = []
		var nodes = el && el.childNodes ? el.childNodes : []
		for (var i = 0; i < nodes.length; i++) {
			var n = nodes[i]
			var last = out[out.length - 1]
			if (n.nodeType === 3 && last && last.nodeType === 3) {
				out[out.length - 1] = { nodeType: 3, nodeValue: String(last.nodeValue || '') + String(n.nodeValue || '') }
			} else if (n.nodeType === 3) {
				out.push({ nodeType: 3, nodeValue: String(n.nodeValue || '') })
			} else {
				out.push(n)
			}
		}
		return out
	}

	function classesOf(el) {
		return String(attr(el, 'class') || '').split(/\s+/).filter(Boolean)
	}

	function isText(n) {
		return n && n.nodeType === 3
	}

	function isElement(n) {
		return n && n.nodeType === 1
	}

	function hasTextOrKept(n) {
		if (isText(n)) return String(n.nodeValue || '').trim() !== ''
		if (!isElement(n)) return false
		var tag = tagOf(n)
		if (VOID_TAGS.indexOf(tag) >= 0 || PRESERVE_EMPTY.indexOf(tag) >= 0) return true
		return children(n).some(hasTextOrKept)
	}

	function hasDataBearingDescendant(el) {
		return children(el).some(function (c) {
			if (!isElement(c) || skipped(c)) return false
			if (DATA_BEARING.indexOf(tagOf(c)) >= 0) return true
			if (['href', 'src', 'value'].some(function (a) { return (attr(c, a) || '').trim() !== '' })) return true
			return hasDataBearingDescendant(c)
		})
	}

	function allText(el, out) {
		children(el).forEach(function (c) {
			if (isText(c)) out.push(String(c.nodeValue || ''))
			else if (isElement(c) && !skipped(c)) allText(c, out)
		})
		return out
	}

	function utf8Length(s) {
		try {
			return new TextEncoder().encode(s).length
		} catch (e) {
			return s.length * 3
		}
	}

	// 천 단위 쉼표를 지운다 (숫자 사이의 쉼표가 정확히 세 자리씩일 때만)
	function stripThousands(s) {
		return s.replace(/(\d),(?=\d{3}(?!\d))/g, '$1')
	}

	function cleanText(t) {
		return stripThousands(t.replace(/"/g, "'").split(/\s+/).filter(Boolean).join(' '))
	}

	function meaningful(el) {
		var attrs = el.attributes ? toArray(el.attributes) : []
		return attrs.some(function (a) {
			var k = String(a.name).toLowerCase()
			return MEANINGFUL_ATTRS.indexOf(k) >= 0 || k.indexOf('data-') === 0
		})
	}

	function emit(node, level, out) {
		if (level > MAX_DEPTH) return
		var indent = new Array(level + 1).join(INDENT)
		if (isText(node)) {
			var t = String(node.nodeValue || '').trim()
			if (t) out.push(indent + '| ' + cleanText(t))
			return
		}
		if (!isElement(node) || skipped(node)) return
		var tag = tagOf(node)
		var kids = children(node)
		var valid = kids.filter(hasTextOrKept)
		if (valid.length === 0 && VOID_TAGS.indexOf(tag) < 0 && PRESERVE_EMPTY.indexOf(tag) < 0) return
		if (WRAPPERS.indexOf(tag) >= 0 && !meaningful(node) && valid.length === 1) {
			kids.forEach(function (c) { emit(c, level, out) })
			return
		}
		// 입력 칸 · 글 상자의 값은 보내지 않는다
		if (tag === 'input' || tag === 'textarea') {
			out.push(indent + tag)
			return
		}
		if (!hasDataBearingDescendant(node)) {
			var clean = allText(node, []).join(' ').trim()
			if (clean && clean.indexOf('\n') < 0 && utf8Length(clean) < INLINE_MAX_BYTES) {
				out.push(indent + tag + ' | ' + cleanText(clean))
				return
			}
		}
		out.push(indent + tag)
		kids.forEach(function (c) { emit(c, level + 1, out) })
	}

	function sanitizeLlm(text) {
		return text
			.replace(/[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F-\u009F﻿​-‍⁠‪-‮⁦-⁩-]/g, '')
			.split('<|').join('< |')
			.split('|>').join('| >')
	}

	function looksPrivate(word) {
		var at = word.indexOf('@')
		var email = at > 0 && word.slice(at + 1).indexOf('.') >= 0
		return email || (word.match(/[0-9]/g) || []).length >= 4
	}

	// 글의 이메일 · 숫자 네 개 이상 낱말을 '#' 로 (서버 · Rust 의 scrub 과 같다)
	function scrubText(text) {
		return text.split('\n').map(function (line) {
			var indent = (line.match(/^ */) || [''])[0]
			return indent + line.slice(indent.length).split(' ').map(function (w) { return looksPrivate(w) ? '#' : w }).join(' ')
		}).join('\n')
	}

	// 요소 → 속성 없는 PUG (contracts/analytic_pug_v1.json). 개인정보 거르기는 보내기 전에 따로 한다(element)
	function toPug(el) {
		var out = []
		emit(el, 0, out)
		if (out.length === 0) return ''
		return sanitizeLlm(out.join('\n') + '\n')
	}

	// ---- 셀렉터용 조상 모양 (crates/premium-discount-nav dom.rs 의 DomNode) ----

	function nthOfType(el) {
		var n = 1
		var tag = tagOf(el)
		for (var s = el.previousElementSibling; s; s = s.previousElementSibling) {
			if (tagOf(s) === tag) n++
		}
		return n
	}

	function domChain(el) {
		var out = []
		for (var cur = el; cur && isElement(cur) && out.length < SELECTOR_DEPTH; cur = cur.parentElement) {
			var tag = tagOf(cur)
			if (tag === 'html' || tag === 'body') break
			var node = { tag: tag, nth_of_type: nthOfType(cur) }
			if (cur.id) node.id = String(cur.id).slice(0, 80)
			var classes = classesOf(cur).slice(0, 8)
			if (classes.length) node.classes = classes
			var attrs = []
			CHAIN_ATTRS.forEach(function (a) {
				var v = attr(cur, a)
				if (v) attrs.push([a, String(v).slice(0, 80)])
			})
			if (attrs.length) node.attrs = attrs
			out.push(node)
		}
		return out
	}

	function element(el) {
		return { chain: domChain(el), pug: scrubText(toPug(el)) }
	}

	// ---- 같은 목록의 형제 (tracker-logis-center 의 목록 찾기를 그대로) ----

	function cleanClassList(el) {
		if (!el) return []
		return classesOf(el)
			.filter(function (c) {
				return IGNORED_CLASSES.indexOf(c.toLowerCase()) < 0 && c.indexOf('__') === -1
			})
			.sort()
	}

	function visible(el) {
		try {
			var cs = root.getComputedStyle(el)
			if (!cs || cs.display === 'none' || cs.visibility === 'hidden' || Number(cs.opacity) === 0) return false
			var r = el.getBoundingClientRect()
			return r.width !== 0 && r.height !== 0
		} catch (e) {
			return false
		}
	}

	function signature(el, n) {
		return tagOf(el) + '|' + cleanClassList(el).slice(0, n).join(',')
	}

	function detectDivListParent(el) {
		var cur = el
		for (var i = 0; i < 6 && cur; i++) {
			var kids = toArray(cur.children || []).filter(api.visible)
			if (kids.length < 3) {
				cur = cur.parentElement
				continue
			}
			var counts = {}
			kids.forEach(function (c) {
				var s = signature(c, 3)
				counts[s] = (counts[s] || 0) + 1
			})
			var top = Object.keys(counts).reduce(function (m, k) { return Math.max(m, counts[k]) }, 0)
			// 가장 흔한 모양이 60 % 를 넘으면 목록의 부모로 본다
			if (top / kids.length > 0.6) return cur
			cur = cur.parentElement
		}
		return null
	}

	function selectListParent(target) {
		return (target.closest && target.closest('ul,ol,dl')) || detectDivListParent(target)
	}

	function similar(el, pattern) {
		var sig = signature(el, 4).split('|')[1]
		var parts = sig ? sig.split(',') : []
		var want = signature(pattern, 4).split('|')[1]
		var wanted = want ? want.split(',') : []
		var match = 0
		wanted.forEach(function (p) {
			if (p !== '' && parts.indexOf(p) >= 0) match++
		})
		return (wanted.length ? (match / wanted.length) * 100 : 0) >= LIST_THRESHOLD
	}

	function collectListItems(list, patternEl) {
		var kids = toArray(list.children || []).filter(api.visible)
		if (kids.length > 1) {
			return kids.filter(function (c) { return similar(c, patternEl) }).slice(0, MAX_RELATED)
		}
		var cur = patternEl.parentElement
		for (var depth = 0; depth < 6 && cur; depth++) {
			var all = toArray(cur.querySelectorAll(tagOf(patternEl))).filter(api.visible)
			if (all.length > 1) {
				var matched = all.filter(function (c) { return similar(c, patternEl) })
				if (matched.length > 1) return matched.slice(0, MAX_RELATED)
			}
			cur = cur.parentElement
		}
		return [patternEl]
	}

	// 목록 안의 요소면 그 항목(목록의 자식)과 고르지 않은 형제들
	function listContext(el) {
		var list = selectListParent(el)
		if (!list) return { item: el, related: [] }
		var item = el
		while (item && item.parentElement && item.parentElement !== list) item = item.parentElement
		if (!item || item.parentElement !== list) item = el
		var related = collectListItems(list, item).filter(function (c) { return c !== item })
		return { item: item, related: related }
	}

	// ---- 크기 어림 (tracker-logis-center 와 같은 식) ----

	function approximateTokenCount(text) {
		if (!text) return 0
		var length = text.length
		var ascii = (text.match(/[\x00-\x7F]/g) || []).length
		var spaces = (text.match(/\s/g) || []).length
		var punctuation = (text.match(/[.,!?;:()\[\]{}"'`~\-_/\\]/g) || []).length
		var digits = (text.match(/\d/g) || []).length
		var total = ascii / 4 + (length - ascii) * 0.8 + spaces * 0.3 + punctuation * 0.5 + digits * 0.35
		return Math.ceil(total * 1.1)
	}

	// 너무 큰 사건은 형제를 반씩 줄이고, 그래도 크면 버린다 (원본은 바로 버렸다)
	function fitBudget(ev) {
		while (approximateTokenCount(JSON.stringify(ev)) * 5 > TOKEN_BUDGET) {
			if (!ev.related || ev.related.length === 0) return null
			ev.related = ev.related.slice(0, Math.floor(ev.related.length / 2))
			if (ev.related.length === 0) delete ev.related
		}
		return ev
	}

	api.VERSION = VERSION
	api.toPug = toPug
	api.domChain = domChain
	api.element = element
	api.cleanClassList = cleanClassList
	api.selectListParent = selectListParent
	api.collectListItems = collectListItems
	api.listContext = listContext
	api.approximateTokenCount = approximateTokenCount
	api.fitBudget = fitBudget
	api.scrubText = scrubText
	api.visible = visible
	root.PDTracker = api

	// ---- 수집 (시험에서는 PD_TRACKER_TEST 로 끈다) ----

	if (root.PD_TRACKER_TEST) return
	var doc = root.document
	var nav = root.navigator || {}
	if (!doc || nav.globalPrivacyControl === true || nav.doNotTrack === '1' || root.doNotTrack === '1') return

	var script = doc.currentScript
	var endpoint = (script && script.getAttribute('data-endpoint')) || 'https://premium.discount/analytic/events'

	function store(key, make) {
		try {
			var v = root.sessionStorage.getItem(key)
			if (v === null) {
				v = make()
				root.sessionStorage.setItem(key, v)
			}
			return v
		} catch (e) {
			return make()
		}
	}

	function randomId() {
		var b = new Uint8Array(16)
		if (root.crypto && root.crypto.getRandomValues) root.crypto.getRandomValues(b)
		else for (var i = 0; i < 16; i++) b[i] = Math.floor(Math.random() * 256)
		return Array.prototype.map.call(b, function (x) { return ('0' + x.toString(16)).slice(-2) }).join('')
	}

	var sid = store('pd.tracker.sid', randomId)
	var started = Number(store('pd.tracker.start', function () { return String(Date.now()) }))
	var seq = Number(store('pd.tracker.seq', function () { return '0' }))
	var queue = []
	var timer = null
	var dirty = []

	// 쿼리 · 문서 안 위치를 뺀 주소 (해시 라우팅 #/… 만 남긴다). 서버도 한 번 더 뺀다
	function href() {
		var l = root.location
		if (!l) return ''
		var h = String(l.hash || '')
		var route = h.indexOf('#/') === 0 || h.indexOf('#!/') === 0 ? h.split('?')[0] : ''
		return String(l.origin || '') + String(l.pathname || '') + route
	}

	// 보내지 않는 대상: 수집에서 뺀 영역 안, 또는 문서 전체(html · body: 배경에 머문 것이라 대상이 없고, PUG 가 페이지 전체가 된다)
	function ignored(el) {
		var tag = tagOf(el)
		return !el || !el.closest || tag === 'html' || tag === 'body' || el.closest('[data-pd-ignore]') !== null
	}

	function send(batch, leaving) {
		var body = JSON.stringify({ events: batch })
		try {
			if (root.fetch) {
				root.fetch(endpoint, { method: 'POST', body: body, keepalive: leaving, mode: 'cors', credentials: 'omit', headers: { 'Content-Type': 'text/plain;charset=UTF-8' } }).catch(function () {})
				return
			}
		} catch (e) {}
		try {
			if (nav.sendBeacon) nav.sendBeacon(endpoint, body)
		} catch (e) {}
	}

	// 떠날 때(keepalive)는 본문 한도(64 KB)가 있어 작게 나눠 보낸다
	function flush(leaving) {
		if (timer) {
			clearTimeout(timer)
			timer = null
		}
		while (queue.length) send(queue.splice(0, leaving ? LEAVING_BATCH : MAX_BATCH), leaving)
	}

	function push(type, el, extra) {
		seq++
		try {
			root.sessionStorage.setItem('pd.tracker.seq', String(seq))
		} catch (e) {}
		var ev = { type: type, href: href(), sid: sid, seq: seq, t_ms: Date.now() - started }
		if (el) {
			// target = 누른 요소(체크포인트), item = 그 요소가 속한 목록 항목(형제와 같은 단위: 선택 대조), related = 고르지 않은 형제
			var ctx = listContext(el)
			ev.target = element(el)
			if (ctx.related.length) {
				if (ctx.item !== el) ev.item = element(ctx.item)
				ev.related = ctx.related.map(element)
			}
		}
		for (var k in extra) if (Object.prototype.hasOwnProperty.call(extra, k)) ev[k] = extra[k]
		ev = fitBudget(ev)
		if (!ev) return
		queue.push(ev)
		if (queue.length >= MAX_BATCH) flush(false)
		else if (!timer) timer = setTimeout(function () { flush(false) }, FLUSH_MS)
	}

	// 페이지에 들어옴
	push('enter', null, {})

	// 누름. 같은 요소를 700 ms 안에 세 번 이상 누르면 광클 (premium-discount-nav 의 rage_presses 처럼 같은 곳만 센다.
	// tracker-logis-center 는 아무 곳이나 세어, 다른 버튼을 연달아 눌러도 광클이 됐다)
	var presses = []
	doc.addEventListener('click', function (e) {
		var el = e.target
		if (!isElement(el) || ignored(el)) return
		var now = Date.now()
		presses = presses.filter(function (p) { return now - p.t < RAGE_WINDOW_MS })
		presses.push({ t: now, el: el })
		var same = presses.filter(function (p) { return p.el === el }).length
		push('click', el, same >= RAGE_PRESSES ? { rage: true } : {})
	}, true)

	// 머묾 (스크롤 중이 아닐 때, 500 ms 이상 머물다 떠남)
	var scrolling = false
	var scrollTimer = null
	var hoverEl = null
	var hoverAt = 0
	doc.addEventListener('mouseover', function (e) {
		if (scrolling) return
		hoverEl = e.target
		hoverAt = Date.now()
	}, true)
	doc.addEventListener('mouseout', function (e) {
		if (!hoverEl || e.target !== hoverEl) return
		var dwell = Date.now() - hoverAt
		var el = hoverEl
		hoverEl = null
		if (dwell >= HOVER_MIN_MS && isElement(el) && !ignored(el)) push('hover', el, { dwell_ms: dwell })
	}, true)

	// 스크롤 묶음 (멈춘 뒤 200 ms). 방향을 뒤집은 수를 센다
	var lastY = root.scrollY || 0
	var lastDir = 0
	var flips = 0
	root.addEventListener('scroll', function () {
		scrolling = true
		var y = root.scrollY || 0
		var dir = y > lastY ? 1 : y < lastY ? -1 : 0
		if (dir !== 0 && lastDir !== 0 && dir !== lastDir) flips++
		if (dir !== 0) lastDir = dir
		lastY = y
		clearTimeout(scrollTimer)
		scrollTimer = setTimeout(function () {
			scrolling = false
			if (flips > 0) push('scroll', null, { flips: flips })
			flips = 0
			lastDir = 0
		}, SCROLL_STOP_DELAY)
	}, { passive: true })

	// 바꿈 (칸마다 600 ms 디바운스). 값은 보내지 않고 칸의 종류만. 비밀번호 칸은 사건이 없다.
	// tracker-logis-center 는 디바운스가 하나라 600 ms 안에 다른 칸을 바꾸면 앞 칸의 사건이 사라졌다
	var changeTimers = []
	doc.addEventListener('change', function (e) {
		var el = e.target
		var tag = tagOf(el)
		if (!(tag === 'select' || tag === 'textarea' || tag === 'input') || ignored(el)) return
		var type = String(el.type || '').toLowerCase()
		if (type === 'password') return
		if (dirty.indexOf(el) < 0) dirty.push(el)
		var slot = changeTimers.filter(function (c) { return c.el === el })[0]
		if (!slot) {
			slot = { el: el, timer: null }
			changeTimers.push(slot)
		}
		clearTimeout(slot.timer)
		slot.timer = setTimeout(function () {
			changeTimers = changeTimers.filter(function (c) { return c !== slot })
			push('change', el, { field: tag === 'input' ? (type || 'text') : tag })
		}, CHANGE_DELAY)
	}, true)

	// 제출
	doc.addEventListener('submit', function (e) {
		var form = e.target
		if (ignored(form)) return
		dirty = []
		push('submit', isElement(form) ? form : null, {})
	}, true)

	// 뒤로
	root.addEventListener('popstate', function () {
		push('back', null, {})
	})

	// 떠남: 바꾸고 제출하지 않은 칸 수와 함께, 남은 사건을 keepalive 로 보낸다
	root.addEventListener('pagehide', function () {
		push('leave', null, dirty.length ? { dirty: dirty.length } : {})
		flush(true)
	})

	api.flush = flush
	api.queue = queue
})(typeof window !== 'undefined' ? window : globalThis)
