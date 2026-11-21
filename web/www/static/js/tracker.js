/*
 * premium.discount 웹 행동 분석 수집 스크립트 (web-event/1). tracker-logis-center 의 tracker.js 를 옮긴 것이다.
 *
 * 사이트 주인이 자기 사이트에 심는다:
 *   <script src="https://premium.discount/static/js/tracker.js" async></script>
 *   (보내는 곳을 바꾸려면 data-endpoint="https://…/analytic/events")
 * 사이트 확인: 첫 화면 <head> 에 <meta name="premium-discount-site" content="<지갑 주소>"> 를 두고, 앱에서 사이트를 등록한다.
 *
 * 무엇을 보내는가: 누름 · 머묾 · 바꿈 · 스크롤 · 제출 · 뒤로 · 떠남 사건과, 그 요소의 "속성 없는 PUG"(태그와 글만) ·
 * 셀렉터용 조상 모양(태그 · id · class · role/name/type/시험용 속성 · 같은 태그 중 순서), 같은 목록에서 고르지 않은 형제
 * (뷰포트에 보인 것만, 고른 항목에서 가까운 순으로 고른 것 포함 16개까지). 글이 없는 아이콘 단추 · 링크는 이름표(aria-label · title ·
 * 그림의 alt)를 `button | [장바구니]` 처럼 보낸다.
 * 보내지 않는 것: 입력값(비밀번호 칸은 사건도 없음), 링크 · 그림 주소, 쿠키, 쿼리 문자열. 글의 이메일 · 숫자 네 개 이상은 '#'.
 * 방문자를 알아보는 값은 탭마다 새로 만드는 무작위 sid 와, 새 탭이 부모 탭에서 물려받는 무작위 탭 묶음(tab_group, 여러 탭으로
 * 견준 방문을 묶는다) 뿐이다. 둘 다 sessionStorage 에 있어 탭을 닫으면 사라진다. GPC · DNT 를 켠 방문자는 수집하지 않는다.
 * data-pd-ignore 속성을 단 요소 안의 사건은 보내지 않는다.
 * 주소가 그대로인 채 SPA 모달 · 드로어(aria-modal · role="dialog" · <dialog open>, 고정 층, 뷰포트의 15 % 이상)가 열리면 가상 주소
 * `#/__modal/<cart | checkout | …>` 로 들어옴을 내고, 모달 위의 사건에는 surface: "modal_drawer" 를 붙인다(퍼널의 장바구니 · 결제 단계).
 * 떠날 때는 결말 사건(떠남 · 제출 · 뒤로 · 마지막 누름)을 첫 묶음(< 8 KB)에 먼저 넣고, 나머지는 keepalive 한도(합 64 KB) 안에서만 보낸다.
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
	// 선택 대조 후보 상한 (고른 것 포함): 뷰포트에 반 이상 보인 형제만, 고른 항목에서 가까운 순
	var MAX_COMPETING_PEERS = 16
	var VIEWPORT_SHARE = 0.5
	// 모달 · 드로어가 뷰포트의 이만큼 이상을 덮어야 화면이 바뀐 것으로 본다
	var MODAL_AREA_SHARE = 0.15
	var MODAL_CHECK_MS = 150
	var MODAL_ROUTE = '__modal'
	var MODAL_SELECTOR = '[aria-modal="true"],[role="dialog"],[role="alertdialog"],dialog[open]'
	// 떠날 때: keepalive 본문 합의 상한(브라우저 한도 64 KB 에서 여유를 뺌), 첫 묶음(결말 사건)의 상한
	var KEEPALIVE_BYTES = 60000
	var FIRST_PACKET_BYTES = 8000
	var TERMINAL_EVENTS = 4
	var TERMINAL_RELATED = 8
	// 글 없는 대화형 요소의 이름표 글자 수
	var LABEL_CHARS = 40
	var INTERACTIVE_TAGS = ['button', 'a']
	var INTERACTIVE_ROLES = ['button', 'link', 'tab', 'menuitem']
	// 모달 이름을 고르는 줄기 (사전 analytic_page_filters · analytic_action_filters 의 cart · checkout)
	var CART_WORDS = ['장바구니', 'cart', 'basket', 'カート', '购物车', 'warenkorb', 'panier', 'carrito', 'cesta', 'carrello', 'carrinho', 'winkelwagen', 'košík']
	var CHECKOUT_WORDS = ['결제', '주문', 'checkout', 'payment', '決済', '注文', '结算', '支付', 'kasse', 'paiement', 'pago', 'pagamento', 'afrekenen', 'pokladna']

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
			// 고정 · 절대 배치 층은 숨은 광고 층으로 본다. 열린 모달 · 드로어의 뿌리는 아니다
			if (s.indexOf('position') >= 0 && (s.indexOf('absolute') >= 0 || s.indexOf('fixed') >= 0) && !modalRoot(el)) return true
		}
		if (tag === 'img' && (attr(el, 'src') || '').indexOf('base64') >= 0) return true
		return tag === 'option' && !isSelectedOption(el)
	}

	// 열린 모달 · 드로어의 뿌리 (aria-modal="true", role="dialog" · "alertdialog", <dialog open>)
	function modalRoot(el) {
		var role = String(attr(el, 'role') || '').trim().toLowerCase()
		return String(attr(el, 'aria-modal') || '').trim().toLowerCase() === 'true' || role === 'dialog' || role === 'alertdialog' || (tagOf(el) === 'dialog' && attr(el, 'open') !== null)
	}

	// 누르는 요소 (글이 없으면 이름표로 대신한다). 입력 칸 · 글 상자 · 선택 상자는 아니다(값을 내지 않는다)
	function interactive(el) {
		var role = String(attr(el, 'role') || '').trim().toLowerCase()
		return INTERACTIVE_TAGS.indexOf(tagOf(el)) >= 0 || INTERACTIVE_ROLES.indexOf(role) >= 0
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

	// 내보낼 글이나 남기는 자식(빈 칸 · 입력 · 그림 등)이 하나라도 있는가. 내보내지 않는 요소(아이콘 svg · base64 그림)는 보지 않는다
	function hasVisibleContent(el) {
		return children(el).some(function (c) {
			if (isText(c)) return String(c.nodeValue || '').trim() !== ''
			if (!isElement(c) || skipped(c)) return false
			var t = tagOf(c)
			return VOID_TAGS.indexOf(t) >= 0 || PRESERVE_EMPTY.indexOf(t) >= 0 || hasVisibleContent(c)
		})
	}

	// 글 없는 대화형 요소의 이름표: aria-label → title → 바로 아래 img[alt] 중 처음 것. 개인정보를 거른 뒤(거르고 나서 자른다) 40자까지
	function fallbackLabel(el) {
		var alt = null
		children(el).some(function (c) {
			if (isElement(c) && tagOf(c) === 'img' && String(attr(c, 'alt') || '').trim()) {
				alt = attr(c, 'alt')
				return true
			}
			return false
		})
		var raw = [attr(el, 'aria-label'), attr(el, 'title'), alt].map(function (s) { return cleanText(String(s || '')) }).filter(Boolean)[0]
		return raw ? Array.from(scrubText(raw)).slice(0, LABEL_CHARS).join('').trim() : ''
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
		// 글 없는 아이콘 단추 · 링크: 이름표를 [ ] 에 넣는다 (Rust html_to_pug 와 같은 규칙)
		if (interactive(node) && !hasVisibleContent(node)) {
			var label = fallbackLabel(node)
			if (label) {
				out.push(indent + tag + ' | [' + label + ']')
				return
			}
		}
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

	// 목록 안의 요소면 그 항목(목록의 자식)과 고르지 않은 형제들 (뷰포트에 보인 것만, competingPeers)
	function listContext(el) {
		var list = selectListParent(el)
		if (!list) return { item: el, related: [] }
		var item = el
		while (item && item.parentElement && item.parentElement !== list) item = item.parentElement
		if (!item || item.parentElement !== list) item = el
		return { item: item, related: competingPeers(collectListItems(list, item), item) }
	}

	// 선택 대조의 후보: 뷰포트에 반 이상 보인 형제만, 고른 항목에서 가까운 순으로 고른 것 포함 MAX_COMPETING_PEERS 개까지, 문서
	// 순서대로. 보지 못한 아래쪽 카드(무한 스크롤)는 고르지 않은 것이 아니다. 배치를 모르면(크기 0) 보인 것으로 친다
	function competingPeers(items, item) {
		var at = items.indexOf(item)
		var seen = []
		items.forEach(function (c, i) {
			if (c === item) return
			var share = api.viewportShare(c)
			if (share === null || share >= VIEWPORT_SHARE) seen.push({ el: c, i: i })
		})
		if (at >= 0) seen.sort(function (a, b) { return Math.abs(a.i - at) - Math.abs(b.i - at) || a.i - b.i })
		return seen.slice(0, MAX_COMPETING_PEERS - 1).sort(function (a, b) { return a.i - b.i }).map(function (x) { return x.el })
	}

	function viewportBox(el) {
		var vw = root.innerWidth
		var vh = root.innerHeight
		var r = el.getBoundingClientRect()
		var w = Math.max(0, Math.min(r.right, vw) - Math.max(r.left, 0))
		var h = Math.max(0, Math.min(r.bottom, vh) - Math.max(r.top, 0))
		return { vw: vw, vh: vh, area: r.width * r.height, inside: w * h }
	}

	// 요소 자신 중 뷰포트 안에 든 몫 (0 ~ 1). 배치를 모르면(시험의 linkedom, 크기 0) null
	function viewportShare(el) {
		try {
			var b = viewportBox(el)
			return b.vw > 0 && b.vh > 0 && b.area > 0 ? b.inside / b.area : null
		} catch (e) {
			return null
		}
	}

	// 뷰포트 중 요소가 덮는 몫 (0 ~ 1). 모르면 null
	function viewportArea(el) {
		try {
			var b = viewportBox(el)
			return b.vw > 0 && b.vh > 0 ? b.inside / (b.vw * b.vh) : null
		} catch (e) {
			return null
		}
	}

	// ---- SPA 모달 · 드로어 (주소는 그대로인데 화면이 바뀐 것) ----

	// 고정 · 절대 배치 층인가 (계산 스타일, 없으면 style 속성)
	function fixedLayer(el) {
		var p = ''
		try {
			p = root.getComputedStyle ? String(root.getComputedStyle(el).position || '') : ''
		} catch (e) {}
		if (!p) {
			var s = String(attr(el, 'style') || '').toLowerCase()
			if (s.indexOf('position') >= 0) p = s.indexOf('fixed') >= 0 ? 'fixed' : s.indexOf('absolute') >= 0 ? 'absolute' : ''
		}
		return p === 'fixed' || p === 'absolute'
	}

	// 지금 열린 모달 · 드로어: 모달의 뿌리이고, 보이고, 고정 · 절대 배치(또는 열린 dialog)이고, 뷰포트의 15 % 이상을 덮는 것 중
	// 문서에서 마지막(위에 뜬) 것. 수집에서 뺀 영역 안의 것은 보지 않는다
	function activeModal(doc) {
		var found = null
		toArray(doc && doc.querySelectorAll ? doc.querySelectorAll(MODAL_SELECTOR) : []).forEach(function (el) {
			if (el.closest && el.closest('[data-pd-ignore]')) return
			if (!api.visible(el) || !(tagOf(el) === 'dialog' || api.fixedLayer(el))) return
			var area = api.viewportArea(el)
			if (area !== null && area < MODAL_AREA_SHARE) return
			found = el
		})
		return found
	}

	// 글이 장바구니 · 결제를 말하면 그 이름
	function slugOfText(text) {
		var t = String(text || '').toLowerCase()
		var has = function (words) { return words.some(function (w) { return t.indexOf(w) >= 0 }) }
		return has(CART_WORDS) ? 'cart' : has(CHECKOUT_WORDS) ? 'checkout' : ''
	}

	// 모달의 제목: aria-label → aria-labelledby 가 가리키는 글 → 첫 제목(h1 ~ h3, role="heading")
	function modalTitle(el) {
		var label = String(attr(el, 'aria-label') || '').trim()
		if (label) return label
		var by = String(attr(el, 'aria-labelledby') || '').trim()
		var doc = el.ownerDocument
		if (by && doc && doc.getElementById) {
			var named = by.split(/\s+/).map(function (id) {
				var n = doc.getElementById(id)
				return n ? String(n.textContent || '') : ''
			}).join(' ').trim()
			if (named) return named
		}
		var h = el.querySelector ? el.querySelector('h1,h2,h3,[role="heading"]') : null
		return h ? String(h.textContent || '').trim() : ''
	}

	// 소문자 영문 · 숫자 · '-' 만, 32자까지, 비면 dialog (Rust event::modal_slug 와 같은 규칙)
	function slugify(raw) {
		var s = String(raw || '').replace(/[A-Z]/g, function (c) { return c.toLowerCase() }).replace(/[^a-z0-9]+/g, '-').replace(/^-+/, '').replace(/-+$/, '')
		return s.slice(0, 32).replace(/-+$/, '') || 'dialog'
	}

	// 컨테이너의 이름: 안정된 id → class → role → 태그
	function selectorSlug(el) {
		var stable = function (t) { return /^[A-Za-z][\w-]{0,39}$/.test(t) && (t.match(/[0-9]/g) || []).length < 4 }
		var id = String(attr(el, 'id') || '')
		if (stable(id)) return slugify(id)
		var cls = classesOf(el).filter(stable)[0]
		return slugify(cls || attr(el, 'role') || tagOf(el))
	}

	// 가상 주소의 이름: 제목이 장바구니 · 결제면 cart · checkout, 아니면 안의 단추 글로, 그래도 아니면 컨테이너 셀렉터에서
	function modalSlug(el) {
		var byTitle = slugOfText(modalTitle(el))
		if (byTitle) return byTitle
		var buttons = toArray(el.querySelectorAll ? el.querySelectorAll('button,a,[role="button"]') : []).map(function (b) {
			return String(b.textContent || '') + ' ' + String(attr(b, 'aria-label') || '')
		}).join(' ')
		return slugOfText(buttons) || selectorSlug(el)
	}

	// 모달의 가상 주소 (Rust event::modal_href 와 같은 규칙): 해시 라우팅이 없으면 <주소>#/__modal/<slug>, 있으면
	// <주소>#/<경로>/__modal/<slug> (이미 모달 주소면 모달 조각을 바꾼다). base 는 쿼리 · 문서 안 위치를 뺀 주소다
	function modalHref(base, slug) {
		var b = String(base || '')
		var hash = b.indexOf('#')
		var name = slugify(slug)
		if (hash < 0) return b + '#/' + MODAL_ROUTE + '/' + name
		var route = b.slice(hash + 1)
		var marker = route.indexOf('/' + MODAL_ROUTE + '/')
		if (marker >= 0) route = route.slice(0, marker)
		return b.slice(0, hash) + '#' + route.replace(/\/+$/, '') + '/' + MODAL_ROUTE + '/' + name
	}

	// ---- 떠날 때 보낼 묶음 ----

	function withRelated(e, n) {
		var c = {}
		for (var k in e) if (Object.prototype.hasOwnProperty.call(e, k)) c[k] = e[k]
		if (c.related) {
			c.related = c.related.slice(0, n)
			if (!c.related.length) delete c.related
		}
		return c
	}

	// 떠날 때(keepalive) 보낼 묶음들. keepalive 본문은 합쳐 64 KB 까지라, 결말 사건(떠남 · 제출 · 뒤로 · 마지막 누름/바꿈, 최대 4건,
	// 형제 8개까지)을 첫 묶음(< 8 KB)에 먼저 넣고, 남은 사건은 합이 KEEPALIVE_BYTES 안일 때만 4건씩 보낸다(누름 · 바꿈 · 들어옴이
	// 먼저, 머묾 · 스크롤은 뒤). 넘치는 사건은 보내지 않는다. 사건의 seq · t_ms 는 그대로라 다시 보내도 서버의 사건 id 는 같다
	function unloadPackets(events) {
		var size = function (batch) { return utf8Length(JSON.stringify({ events: batch })) }
		var rank = function (e) { return e.type === 'leave' ? 0 : e.type === 'submit' ? 1 : e.type === 'back' ? 2 : 3 }
		var lastAction = -1
		var picked = []
		events.forEach(function (e, i) {
			if (e.type === 'leave' || e.type === 'submit' || e.type === 'back') picked.push(i)
			else if (e.type === 'click' || e.type === 'change') lastAction = i
		})
		if (lastAction >= 0) picked.push(lastAction)
		picked.sort(function (a, b) { return rank(events[a]) - rank(events[b]) || b - a })
		picked = picked.slice(0, TERMINAL_EVENTS)
		var slim = {}
		picked.forEach(function (i) { slim[i] = withRelated(events[i], TERMINAL_RELATED) })
		var head = function () { return picked.slice().sort(function (a, b) { return a - b }).map(function (i) { return slim[i] }) }
		// 첫 묶음을 8 KB 아래로: 형제가 가장 많은 사건의 형제를 반씩 줄이고, 그래도 크면 덜 급한 결말 사건을 뒤 묶음으로 (떠남은 늘 남는다)
		while (picked.length && size(head()) >= FIRST_PACKET_BYTES) {
			var widest = picked.filter(function (i) { return slim[i].related }).sort(function (a, b) { return slim[b].related.length - slim[a].related.length })[0]
			if (widest !== undefined) slim[widest] = withRelated(slim[widest], Math.floor(slim[widest].related.length / 2))
			else if (picked.length > 1) picked.pop()
			else break
		}
		var packets = []
		var used = 0
		if (picked.length) {
			packets.push(head())
			used = size(packets[0])
		}
		var minor = function (e) { return e.type === 'hover' || e.type === 'scroll' ? 1 : 0 }
		var rest = []
		events.forEach(function (e, i) { if (picked.indexOf(i) < 0) rest.push(i) })
		rest.sort(function (a, b) { return minor(events[a]) - minor(events[b]) || a - b })
		var batch = []
		var batchBytes = 0
		rest.forEach(function (i) {
			var next = batch.concat([events[i]])
			var bytes = size(next)
			if (used - batchBytes + bytes > KEEPALIVE_BYTES) return
			used += bytes - batchBytes
			batch = next
			batchBytes = bytes
			if (batch.length >= LEAVING_BATCH) {
				packets.push(batch)
				batch = []
				batchBytes = 0
			}
		})
		if (batch.length) packets.push(batch)
		return packets
	}

	// 새 탭이 부모 탭의 sessionStorage 를 물려받았나: window.open · rel="opener" 로 연 탭의 첫 페이지(새로 고침 아님).
	// 그때는 탭 세션(sid)을 새로 만들고 탭 묶음만 이어 쓴다
	function inheritedTab(hasOpener, historyLength, navigationType) {
		return !!hasOpener && historyLength === 1 && navigationType !== 'reload'
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
	api.competingPeers = competingPeers
	api.viewportShare = viewportShare
	api.viewportArea = viewportArea
	api.fixedLayer = fixedLayer
	api.activeModal = activeModal
	api.modalSlug = modalSlug
	api.modalHref = modalHref
	api.slugify = slugify
	api.unloadPackets = unloadPackets
	api.inheritedTab = inheritedTab
	api.limits = { MAX_COMPETING_PEERS: MAX_COMPETING_PEERS, KEEPALIVE_BYTES: KEEPALIVE_BYTES, FIRST_PACKET_BYTES: FIRST_PACKET_BYTES, TERMINAL_RELATED: TERMINAL_RELATED }
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

	function navigationType() {
		try {
			var entry = root.performance && root.performance.getEntriesByType ? root.performance.getEntriesByType('navigation')[0] : null
			return entry ? String(entry.type || '') : ''
		} catch (e) {
			return ''
		}
	}

	// 부모 탭의 sessionStorage 를 물려받은 새 탭이면 탭 세션은 새로 (탭 묶음 pd.tracker.tg 는 이어 쓴다)
	if (inheritedTab(root.opener, root.history && root.history.length, navigationType())) {
		try {
			['pd.tracker.sid', 'pd.tracker.start', 'pd.tracker.seq'].forEach(function (k) { root.sessionStorage.removeItem(k) })
		} catch (e) {}
	}
	var sid = store('pd.tracker.sid', randomId)
	var tabGroup = store('pd.tracker.tg', randomId)
	var started = Number(store('pd.tracker.start', function () { return String(Date.now()) }))
	var seq = Number(store('pd.tracker.seq', function () { return '0' }))
	var queue = []
	var timer = null
	var dirty = []
	// 열린 모달 · 드로어: { el, href(가상 주소) }
	var modal = null

	// 쿼리 · 문서 안 위치를 뺀 주소 (해시 라우팅 #/… 만 남긴다). 서버도 한 번 더 뺀다
	function docHref() {
		var l = root.location
		if (!l) return ''
		var h = String(l.hash || '')
		var route = h.indexOf('#/') === 0 || h.indexOf('#!/') === 0 ? h.split('?')[0] : ''
		return String(l.origin || '') + String(l.pathname || '') + route
	}

	// 사건의 주소: 모달 · 드로어가 열려 있으면 그 가상 주소
	function href() {
		return modal ? modal.href : docHref()
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

	// 떠날 때(keepalive)는 본문 한도(합 64 KB)가 있어, 결말 사건을 첫 묶음에 먼저 넣고 작게 나눠 보낸다 (unloadPackets)
	function flush(leaving) {
		if (timer) {
			clearTimeout(timer)
			timer = null
		}
		if (leaving) {
			unloadPackets(queue.splice(0, queue.length)).forEach(function (batch) { send(batch, true) })
			return
		}
		while (queue.length) send(queue.splice(0, MAX_BATCH), false)
	}

	function push(type, el, extra) {
		seq++
		try {
			root.sessionStorage.setItem('pd.tracker.seq', String(seq))
		} catch (e) {}
		var ev = { type: type, href: href(), sid: sid, seq: seq, t_ms: Date.now() - started }
		if (modal) ev.surface = 'modal_drawer'
		if (tabGroup) ev.tab_group = tabGroup
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

	// 떠남: 바꾸고 제출하지 않은 칸 수와 함께, 남은 사건을 keepalive 로 보낸다 (떠남이 첫 묶음)
	root.addEventListener('pagehide', function () {
		push('leave', null, dirty.length ? { dirty: dirty.length } : {})
		flush(true)
	})

	// 가려짐(다른 탭 · 앱으로 감. 모바일은 pagehide 없이 닫히기도 한다): 쌓인 사건을 keepalive 로 보낸다. 떠남은 내지 않는다
	doc.addEventListener('visibilitychange', function () {
		if (doc.visibilityState === 'hidden' && queue.length) flush(true)
	})

	// 모달 · 드로어: 열리면 가상 주소로 들어옴을, 닫히면 문서 페이지로 들어옴을 낸다. DOM 이 바뀔 때 150 ms 에 한 번 본다
	function checkModal() {
		var el = activeModal(doc)
		if (el && (!modal || modal.el !== el)) {
			modal = { el: el, href: modalHref(docHref(), modalSlug(el)) }
			push('enter', null, {})
		} else if (!el && modal) {
			modal = null
			push('enter', null, {})
		}
	}
	var modalTimer = null
	function watchModal() {
		if (!modalTimer) modalTimer = setTimeout(function () {
			modalTimer = null
			checkModal()
		}, MODAL_CHECK_MS)
	}
	try {
		if (root.MutationObserver) {
			new root.MutationObserver(watchModal).observe(doc.documentElement || doc, { subtree: true, childList: true, attributes: true, attributeFilter: ['aria-modal', 'open', 'role', 'style', 'class', 'hidden'] })
		}
	} catch (e) {}
	watchModal()

	api.flush = flush
	api.queue = queue
})(typeof window !== 'undefined' ? window : globalThis)
