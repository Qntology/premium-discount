import { ethers } from 'ethers'

import { gzip, ungzip } from 'pako'

import { renderHtml } from './renderHtml'
import { handleEvaluations, type EvalEnv } from './evaluations'
import { handleGraph } from './graph'
import { handleNavTrials } from './navtrials'
import { handleAnalytic, type AnalyticEnv } from './analytic'
import { handleSession } from './session'

/*

	로컬에서 해야할것
	    rust로 스케줄링하기
	        사용 모델  Qwen3.5 0.8B

		embeddinggemma-300m 768차원으로 벡터화하기


	5단계 호가


	참조값 유사값 ID
	- 내부 게시글 내용중 유사한 내용으로 합치기

	유사값 ID (VS 목적)
	- 외부 게시글중 유사한 내용으로 합치기


	CREATE TABLE IF NOT EXISTS items (
	    "id" TEXT PRIMARY KEY, // 의견 아이디
	    "type" INTEGER, // 0:draft, 1:premium, 2:discount
	    "from" TEXT, // 작성AI  address
	    "to" TEXT,   // 주소    address
	    "cc" TEXT,   // 도메인  address
	    "inf" TEXT,  // 참조값  address
	    "ref" TEXT,  // 유사값  address
	    "digest" TEXT, // Digest(title)
	    "data" BLOB NULL, // 글 내용 {}
	    "created_at" INTEGER, // 생성일
	    "updated_at" INTEGER  // 수정일
	);


	{ 최초 작성
	    "0x000000000000...", // 의견 아이디
	    0,
	    "0x111111111111...", gemini address
	    "0x222222222222...",
	    "0x333333333333...",
	    "0x444444444444...",
	    "0x555555555555...",
	    "0x666666666666...",
	    "{
	        "head" : "글제목",
	        "body" : "내용",
	        "link" : "href",

	    }" -> BLOB,
	    "1773529348" INTEGER, // 생성일
	    "1773529348" INTEGER  // 수정일
	}

	{ 분류 완료
	    "0x000000000000..." // 의견 아이디
	    1,
	    "0x111111111111...", gemini address
	    "0x222222222222...",
	    "0x333333333333...",
	    "0x444444444444...",
	    "0x555555555555...",
	    "0x666666666666...",
	    "{
	        "head" : "글제목",
	        "body" : "내용",
	        "link" : "href",

	    }" -> BLOB,
	    "1773529348" INTEGER, // 생성일
	    "1773529348" INTEGER  // 수정일
	}
*/

function getZeroUTC(date, day) {
	var date = new Date(date)

	date.setDate(date.getDate() - day)

	date.setUTCHours(0)
	date.setUTCMinutes(0)
	date.setUTCSeconds(0)
	date.setUTCMilliseconds(0)

	return date.getTime() // 'YYYY-MM-DDTHH:mm:ss.sssZ'
}

function safeClone(obj) {
	const seen = new WeakMap();
	function clone(value) {
		if (typeof value !== "object" || value === null) return value;
		if (seen.has(value)) return null; // 순환 참조 제거
		const copy = Array.isArray(value) ? [] : {};
		seen.set(value, copy);
		for (const key in value) {
			copy[key] = clone(value[key]);
		}
		return copy;
	}
	return clone(obj);
}

const isDiff = (obj1, obj2) => {
	// If both objects are null or undefined, they are not considered different.
	if (!obj1 && !obj2) {
		return false;
	}

	// If one is falsy and the other isn't, they are different.
	if (!obj1 || !obj2) {
		return true;
	}

	const keys1 = Object.keys(obj1);
	const keys2 = Object.keys(obj2);

	// If the number of keys is different, the objects are different.
	if (keys1.length !== keys2.length) {
		return true;
	}

	// Iterate over keys to check for differences.
	for (const key of keys1) {
		// Check for specific buffer comparison for keys named 'data'.
		if (key === 'data' && Buffer.isBuffer(obj1[key]) && Buffer.isBuffer(obj2[key])) {
			// Use Buffer.equals() for efficient byte-by-byte comparison.
			if (!obj1[key].equals(obj2[key])) {
				return true;
			}
		} else if (typeof obj1[key] === 'object' && typeof obj2[key] === 'object') {
			// Recursively call isDiff for nested objects.
			if (isDiff(obj1[key], obj2[key])) {
				return true;
			}
		} else if (obj1[key] !== obj2[key]) {
			// If values are not equal, the objects are different.
			return true;
		}
	}

	// If no differences are found, the objects are the same.
	return false;
};

function hashId(text){
	if(typeof text == "undefined"){
		var account = ethers.Wallet.createRandom()
		text = account.privateKey
	}

	var hashMessage = ethers.hashMessage(text)

	return ethers.computeAddress(hashMessage).toLowerCase()
}

async function Sleep(ms) {
	return new Promise(resolve => setTimeout(resolve, ms))
}

const twoPartDomains = ["co.kr","co.uk","co.jp","com.cn","co.in","com.mx","co.id","com.my","com.sg","com.ph","com.vn"];

const CenterRegion = "center_premium_discount"

const EdgeRegion = {
	// Western North America
	'us-w': 'wnam_premium_discount',
	'ca-w': 'wnam_premium_discount',

	// Eastern North America
	'us': 'enam_premium_discount',
	'ca': 'enam_premium_discount',
	'mx': 'enam_premium_discount',
	'cu': 'enam_premium_discount',
	'do': 'enam_premium_discount',
	'pr': 'enam_premium_discount',
	'jm': 'enam_premium_discount',

	// Western Europe
	'gb': 'weur_premium_discount',
	'ie': 'weur_premium_discount',
	'fr': 'weur_premium_discount',
	'de': 'weur_premium_discount',
	'nl': 'weur_premium_discount',
	'be': 'weur_premium_discount',
	'lu': 'weur_premium_discount',
	'ch': 'weur_premium_discount',
	'at': 'weur_premium_discount',
	'es': 'weur_premium_discount',
	'pt': 'weur_premium_discount',
	'it': 'weur_premium_discount',
	'se': 'weur_premium_discount',
	'no': 'weur_premium_discount',
	'dk': 'weur_premium_discount',
	'fi': 'weur_premium_discount',

	// Eastern Europe
	'ru': 'eeur_premium_discount',
	'pl': 'eeur_premium_discount',
	'cz': 'eeur_premium_discount',
	'hu': 'eeur_premium_discount',
	'ro': 'eeur_premium_discount',
	'bg': 'eeur_premium_discount',
	'ua': 'eeur_premium_discount',
	'gr': 'eeur_premium_discount',
	'rs': 'eeur_premium_discount',

	// Asia_Pacific
	'cn': 'apac_premium_discount',
	'hk': 'apac_premium_discount',
	'kr': 'apac_premium_discount',
	'jp': 'apac_premium_discount',
	'sg': 'apac_premium_discount',
	'tw': 'apac_premium_discount',
	'th': 'apac_premium_discount',
	'vn': 'apac_premium_discount',
	'my': 'apac_premium_discount',
	'ph': 'apac_premium_discount',
	'id': 'apac_premium_discount',
	'in': 'apac_premium_discount',
	'pk': 'apac_premium_discount',
	'bd': 'apac_premium_discount',

	// Oceania
	'au': 'oc_premium_discount',
	'nz': 'oc_premium_discount',
	'fj': 'oc_premium_discount',
	'pg': 'oc_premium_discount',

	// South America
	'br': 'enam_premium_discount', // Brazil
	'ar': 'enam_premium_discount', // Argentina
	'cl': 'enam_premium_discount', // Chile
	'co': 'enam_premium_discount', // Colombia
	'pe': 'enam_premium_discount', // Peru

	// Africa
	'za': 'weur_premium_discount', // South Africa
	'ng': 'weur_premium_discount', // Nigeria
	'eg': 'weur_premium_discount', // Egypt

	// Middle East
	'sa': 'eeur_premium_discount', // Saudi Arabia
	'ae': 'eeur_premium_discount', // United Arab Emirates
	'tr': 'eeur_premium_discount', // Turkey
};



const Hello = {
	"Korean": "안녕하세요 내용을 입력해주세요",
	"Japanese": "こんにちは、内容を入力してください",
	"English": "Hello, please enter the content",
	"Chinese": "你好，请输入内容",
	"French": "Bonjour, veuillez saisir le contenu",
	"German": "Hallo, bitte geben Sie den Inhalt ein",
	"Spanish": "Hola, por favor ingrese el contenido",
	"Russian": "Здравствуйте, пожалуйста, введите содержание",
	"Arabic": "مرحبًا، يرجى إدخال المحتوى"
}

const languageCodeToCountryCode = {
	'ko': 'kr', // Korean -> South Korea
	'ja': 'jp', // Japanese -> Japan
	'en': 'us', // English -> United States (가장 일반적인 영어를 사용하는 국가)
	'zh': 'cn', // Chinese -> China (가장 일반적인 중국어를 사용하는 국가)
	'fr': 'fr', // French -> France
	'de': 'de', // German -> Germany
	'es': 'es', // Spanish -> Spain
	'ru': 'ru', // Russian -> Russia
	'ar': 'sa', // Arabic -> Saudi Arabia
};


const languageCode = {
	// Western North America
	'us-w': 'English',
	'ca-w': 'English',

	// Eastern North America
	'us': 'English',
	'ca': 'English',
	'mx': 'Spanish',
	'cu': 'Spanish',
	'do': 'Spanish',
	'pr': 'Spanish',
	'jm': 'English',

	// Western Europe
	'gb': 'English',
	'ie': 'English',
	'fr': 'French',
	'de': 'German',
	'nl': 'English',
	'be': 'French',
	'lu': 'French',
	'ch': 'German',
	'at': 'German',
	'es': 'Spanish',
	'pt': 'Portuguese',
	'it': 'Italian',
	'se': 'Swedish',
	'no': 'Norwegian',
	'dk': 'Danish',
	'fi': 'Finnish',

	// Eastern Europe
	'ru': 'Russian',
	'pl': 'Polish',
	'cz': 'Czech',
	'hu': 'Hungarian',
	'ro': 'Romanian',
	'bg': 'Bulgarian',
	'ua': 'Ukrainian',
	'gr': 'Greek',
	'rs': 'Serbian',

	// Asia-Pacific
	'cn': 'Simplified Chinese',
	'hk': 'Traditional Chinese',
	'kr': 'Korean',
	'jp': 'Japanese',
	'sg': 'English',
	'tw': 'Traditional Chinese',
	'th': 'Thai',
	'vn': 'Vietnamese',
	'my': 'Malay',
	'ph': 'English',
	'id': 'Indonesian',
	'in': 'English',
	'pk': 'Urdu',
	'bd': 'Bengali',

	// Oceania
	'au': 'English',
	'nz': 'English',
	'fj': 'English',
	'pg': 'English',

	// South America
	'br': 'Portuguese', // Brazil
	'ar': 'Spanish', // Argentina
	'cl': 'Spanish', // Chile
	'co': 'Spanish', // Colombia
	'pe': 'Spanish', // Peru

	// Africa
	'za': 'English', // South Africa
	'ng': 'English', // Nigeria
	'eg': 'Arabic',  // Egypt

	// Middle East
	'sa': 'Arabic', // Saudi Arabia
	'ae': 'Arabic', // United Arab Emirates
	'tr': 'Turkish' // Turkey
}


export default {
	// Method GET, POST
	async fetch(
		request: Request,
		env: Env,
		ctx: ExecutionContext
	): Promise<Response> {
		// 앱 로그인 API (/session*): 기존 로직보다 먼저 처리
		var sessionResponse = await handleSession(request, env as unknown as EvalEnv)
		if(sessionResponse){
			return sessionResponse
		}

		// 평가 시스템 API (/evaluations): 기존 로직보다 먼저 처리
		var evaluationResponse = await handleEvaluations(request, env as unknown as EvalEnv)
		if(evaluationResponse){
			return evaluationResponse
		}

		// 접근성 내비게이션 타임트라이얼 API (/nav/trials): 세션 또는 지갑 서명
		var navResponse = await handleNavTrials(request, env as unknown as EvalEnv)
		if(navResponse){
			return navResponse
		}

		// 웹 행동 분석 수집 (/analytic/events: 수집 스크립트, /analytic/sites: 사이트 주인의 지갑 서명)
		var analyticResponse = await handleAnalytic(request, env as unknown as AnalyticEnv)
		if(analyticResponse){
			return analyticResponse
		}

		// 그래프 API (/graph*, 공개)
		var graphResponse = await handleGraph(request, env as unknown as EvalEnv)
		if(graphResponse){
			return graphResponse
		}

		var {
			// 도시 (예: "San Jose")
			city,
			// 국가 코드 (예: "US")
			country,
			// 국가 이름 (예: "United States")
			countryRegion,
			// 대륙 코드 (예: "NA")
			continent,
			// 위도 (예: "37.33940")
			latitude,
			// 경도 (예: "-121.89496")
			longitude,
			// 시도 (예: "California")
			region,
			// 시도 코드 (예: "CA")
			regionCode,
			// 타임존 (예: "America/Los_Angeles")
			timezone,
			// 우편번호 (예: "95113")
			postalCode,
			// AS 번호 (예: "13335")
			asOrganization,
		} = request.cf || {};

		// 요청자의 IP 주소
		var ip = request.headers.get('cf-connecting-ip');

		if(!ip){
			ip = request.headers.get('X-Real-IP')
		}

		// 응답 본문에 정보를 포함하여 반환
		var geoInfo = {
			ip,
			city,
			country,
			countryRegion,
			continent,
			latitude,
			longitude,
			region,
			regionCode,
			timezone,
			postalCode,
			asOrganization,
		};

		var FLAG = (geoInfo.country || '').toLowerCase()

		// 국가별 DB 바인딩 매핑은 아직 정해지지 않았다. 비어 있으면 아래 바인딩 조회가 try 블록 안에서 실패하고, 요청은 500 대신 418로 끝난다
var regionBindings: Record<string, string> = {}
var userRegion = regionBindings[FLAG]

		var zoneRegion = ''

		var language = languageCode[FLAG]

		var cc = ''

		try{
			var headers = new Headers()

			var contentType = request.headers.get("Content-Type")
			var contentEncoding = request.headers.get("Content-Encoding")


			const requestUrl = new URL(request.url)
			const queryParams = requestUrl.searchParams

			var req = {
				url:request.url,
				host:requestUrl.hostname,
				method:request.method,
				query:{},
				body:{}
			}

			if(queryParams){
				if(queryParams.size){
					queryParams.forEach((value, key) => {
						req.query[key] = value
					})
				}
			}

			const acceptLanguageHeader = request.headers.get('Accept-Language')

			let preferredLanguage = "" // 기본값 설정

			if (acceptLanguageHeader) {
				// 2. 가져온 헤더 값을 파싱하여 가장 선호하는 언어를 추출합니다.
				// 보통 첫 번째 항목이 가장 선호하는 언어입니다.
				// 복잡한 파싱 로직 (q 값 고려)은 필요에 따라 추가할 수 있습니다.

				const languages = acceptLanguageHeader.split(',')
				if (languages.length) {
					// 첫 번째 언어 코드 (예: "ko-KR")만 사용하고, q 값이나 추가 정보는 제거
					preferredLanguage = languages[0].split(';')[0].trim()
				}
			}


			var redirect = "https://www.premium.discount/"

			var origin = decodeURIComponent(req.query.origin ? req.query.origin : "")

			var userAgent = request.headers.get('User-Agent')

			if(ip){
				var m = ip.match(/\b\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}\b/)

				if(m){
					ip = m[0]
				}
			}

			console.log('req.query.referrer',req.query.referrer);
			var href = decodeURIComponent(req.query.href ? req.query.href : "").toLowerCase()

			var url = new URL(href)

			if(origin && href){
				try{
					if(href.indexOf(origin) > -1){
						var currentHost = url.host

						var host = currentHost.split('.')

						var isTwoPartDomain = twoPartDomains.some(domain => {
							return currentHost.endsWith(domain);
						});

						if(isTwoPartDomain){
							cc = host[host.length-3]+"."+host[host.length-2]+"."+host[host.length-1]
						}else{
							cc = host[host.length-2]+"."+host[host.length-1]
						}
					}

				}catch(err){
					console.log('err',err);
				}
			}


			var now = Date.now()

			var current = new Date(now).toISOString()


			headers.set('Access-Control-Allow-Origin', '*')
			headers.set('Access-Control-Allow-Methods', 'GET, POST, OPTIONS')
			headers.set('Access-Control-Allow-Headers', 'Content-Type, Content-Encoding, Authorization, X-Requested-With')

			headers.set('Content-Security-Policy', `frame-ancestors 'self' ${origin};`)

			var created_at = 0

			if(!isNaN(req.query.created_at)){
				created_at = parseInt(req.query.created_at)
			}

			var pathname = url.pathname.toLowerCase()

			var link = (url.pathname+url.search).toLowerCase()

			var pageId = ethers.isAddress(req.query.id) ? req.query.id.toLowerCase() : hashId(cookies.cc+pathname)


			// from  = ex chatgpt or gemini or deepseek or qwen
			// to = pathname
			// cc = host

			// ref = href

			var homeFrom = hashId(ethers.ZeroAddress)

			var homeCc = cc

			var homeId = hashId(homeCc.toUpperCase())

			var homeRef = ''

			var home = {
				id : homeId,
				type : '',
				from : ethers.ZeroAddress,
				to : ethers.ZeroAddress,
				cc : homeCc,
				inf : '',
				ref : '',
				data : {
					origin : url.origin,
					link : href
				},
				created_at : now,
				updated_at : now
			}

			home.data = gzip(new TextEncoder('utf-8').encode(JSON.stringify(home.data)), { to: 'arraybuffer' })

			var tasks = []

			var items = []

			var hints = []

			var detail = {}

			var pages = [home]

			var page = homeId == pageId ? home : {}

			var isDetail = false

			var temp = {}

			var type = homeId == pageId ? 'tracking' : ''


			var users = []

			var admin = {}

			var { results } = await env[`premium_dicount_${zoneRegion}_items`].prepare(`SELECT * FROM items WHERE "to" = '${cookies.team}' AND "ref" = '${itemId}' LIMIT 1`).all()

			// console.log('detail results.length',results.length);
			// console.log(`SELECT * FROM items WHERE "to" = '${cookies.team}' AND "ref" = '${itemId}' LIMIT 1`);

			if(results.length){
				detail = results[0]

				items.push(detail)
			}

			console.log('detail',JSON.stringify(detail))

			var { results } = await env[`premium_dicount_${zoneRegion}_items`].prepare(`SELECT * FROM items WHERE "type" = 'pages' AND "to" = '${cookies.team}' LIMIT 100`).all()

			console.log('page results.length',results.length);

			if(results.length){
				for(var i = 0; i < results.length; i++){
					var _page = results[i]

					var decompressedJsonString = new TextDecoder('utf-8').decode(ungzip(_page.data))

					var page_data = JSON.parse(decompressedJsonString)

					_page.table = 'pages'

					temp[_page.id] = true

					pages.push(_page)
				}
			}

			console.log('Object.keys(page).length',Object.keys(page).length);

			if(Object.keys(page).length == 0){
				var { results } = await env[`premium_dicount_${zoneRegion}_items`].prepare(`SELECT * FROM items WHERE "type" = 'pages' AND "cc" = '${cookies.cc}' AND "created_at" < ${created_at} LIMIT 100`).all()

				if(results.length){
					var statements = []

					for(var i = 0; i < results.length; i++){
						var _page = results[i]

						if(temp[_page.id]){
							continue;
						}

						temp[_page.id] = true

						var decompressedJsonString = new TextDecoder('utf-8').decode(ungzip(_page.data))

						var page_data = JSON.parse(decompressedJsonString)

						var bool = cookies.team == _page.to

						_page.type = page_data.type
						_page.from = cookies.address ? cookies.address : ethers.ZeroAddress
						_page.to = cookies.team ? cookies.team : ethers.ZeroAddress

						_page.table = 'pages'

						if(detail.bcc == _page.bcc){
							_page.current = true
							page = _page

							if(_page.type == "tracking"){
								type = "tracking"

							}else if(_page.type == "goods" || _page.type == "order"){
								type = "sales"

							}else if(_page.type == "event" || _page.type == "coupon"){
								type = "event"

							}

							pageId = page.id


							isDetail = true
						}


						if(Object.keys(detail).length == 0 && pageId == _page.id){
							_page.current = true
							page = _page

							if(_page.type == "tracking"){
								type = "tracking"

							}else if(_page.type == "goods" || _page.type == "order"){
								type = "sales"

							}else if(_page.type == "event" || _page.type == "coupon"){
								type = "event"

							}


							if(Object.keys(_page).length){
								isDetail = page_data.detail
							}
						}

						var arr = gzip(new TextEncoder('utf-8').encode(JSON.stringify(page_data)), { to: 'arraybuffer' })


						if(!bool){
							statements.push(
								env[`premium_dicount_${zoneRegion}_items`].prepare(`
									INSERT INTO items (
										"id", "type", "from", "to", "cc", "bcc", "ref", "digest", "data", "created_at", "updated_at"
									) VALUES (
										?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11
									) ON CONFLICT (id) DO UPDATE SET
										"type" = EXCLUDED."type",
										"from" = EXCLUDED."from",
										"to" = EXCLUDED."to",
										"cc" = EXCLUDED."cc",
										"bcc" = EXCLUDED."bcc",
										"ref" = EXCLUDED."ref",
										"digest" = EXCLUDED."digest",
										"data" = EXCLUDED."data",
										"created_at" = EXCLUDED."created_at",
										"updated_at" = EXCLUDED."updated_at"
								`).bind(
									_page.id,
									'pages',
									_page.from,
									_page.to,
									_page.cc,
									_page.bcc,
									_page.ref,
									_page.digest,
									arr.buffer,
									now,
									now
								)
							)
						}

						_page.data = arr

						pages.push(_page) 

							
					}

					if(statements.length){
						await env[`premium_dicount_${zoneRegion}_items`].batch(statements)
					}
				}else{
					var { results } = await env[CenterRegion].prepare(`SELECT * FROM pages WHERE "id" = '${pageId}' AND "created_at" < ${created_at} LIMIT 1`).all()

					if(results.length){
						page = results[0]

						var decompressedJsonString = new TextDecoder('utf-8').decode(ungzip(page.data))

						var page_data = JSON.parse(decompressedJsonString)

						page.current = true

						page.from = cookies.address ? cookies.address : ethers.ZeroAddress
						page.to = cookies.team ? cookies.team : ethers.ZeroAddress
						page.table = 'pages'

						if(page.type == "goods" || page.type == "order"){
							type = "sales"

						}else if(page.type == "event" || page.type == "coupon"){
							type = "event"

						}

						temp[page.id] = page

						pages.push(page)


						if(Object.keys(page).length){
							isDetail = page_data.detail
						}
					}
				}
					
			}

			if(req.query.crons){
				try{
					var crons = JSON.parse(req.query.crons)

					if(crons.length){
						var cron_condition = ''

						var tasks = []

						var temp = {}

						for(var c = 0; c < crons.length; c++){
							var cron = crons[c].toLowerCase()

							if(ethers.isAddress(cron)){
								cron_condition += ` OR ("id" = '${cron}' AND "from" = '${cookies.address}' AND "created_at" < ${now}) `

								temp[cron] = c

								tasks.push({
									id : cron,
									table : 'tasks',
									type : '',
									from : '',
									to : '',
									cc : '',
									bcc : '',
									ref : '',
									data : null,
									created_at : now,
									updated_at : now
								})
							}
						}

						cron_condition = cron_condition.replace(' OR ','')

						var { results, success } = await env[userRegion].prepare(`SELECT * FROM tasks WHERE ${cron_condition} ORDER BY created_at LIMIT ${crons.length}`).all()
						
						if(results.length){
							for(var i = 0; i < results.length; i++){
								var cron = results[i]

								var index = temp[cron.id]

								if(typeof index != "undefined"){
									tasks.splice(index, 1);
								}
							}
						}
					}
				}catch(err){
					console.log('tasks err',err);
				}		
			}



			if(pages.length){
				try{
					var branchs = []

					var _pages = safeClone(pages)

					for (var p = 0; p < _pages.length; p++) {
						var _page = _pages[p]

						var decompressedJsonString = new TextDecoder('utf-8').decode(ungzip(_page.data))

						_page.data = JSON.parse(decompressedJsonString)

						var _url = new URL(_page.data.origin)

						if(_page.data.item){
							branchs[`${_page.data.origin}#${_page.type}`] = { ..._page, children: [] }
						}

						branchs[_page.id] = { ..._page, children: [] }
					}


					var _temp = {}

					for(var key in branchs){
						if (branchs.hasOwnProperty(key)) {
							var _page = safeClone(branchs[key])

							if(!_temp[_page.id]){
								_temp[_page.id] = true

								var parent = branchs[`${_page.data.origin}#${_page.type}`]

								if(parent){
									if(_page.data.item){
										var children = safeClone(parent.children)

										branchs[`${_page.data.origin}#${_page.type}`] = {
											..._page,
											children : children
										}
									}else{
										branchs[`${_page.data.origin}#${_page.type}`].children.push(_page)
									}
									
								}else{
									if(_page.data.item){
										if(!branchs[`${_page.data.origin}#${_page.type}`]){
											branchs[`${_page.data.origin}#${_page.type}`] = {
												..._page,
												children : []
											}
										}
									}else{
										branchs[`${_page.data.origin}#${_page.type}`].children.push(_page)
									}
								}
							}
						}
					}

					// console.log('branchs',JSON.stringify(branchs));

					var tree = []

					for(var key in branchs){
						if (branchs.hasOwnProperty(key)) {
							if(!ethers.isAddress(key)){
								var _page = branchs[key]

								tree.push(_page)
							}
						}
					}

					// console.log('tree',JSON.stringify(tree));	
				}catch(err){

				}

			}



			if(request.method == 'PUT'){
				var success = false

				var results = []


				headers.set('Content-Type', 'application/json')

				return new Response(JSON.stringify({
					results:results,
					success:success
				}), {
					headers:headers
				})

			}else if(request.method == 'POST') {
				// POST 바디 파싱
				var body = await request.json()

				// -------------------------------------------------------------
				// [분기 1] 로컬 Rust AI (스케줄러)가 분석을 마치고 결과를 업데이트하는 경우
				// -------------------------------------------------------------
				if(body.action === "update_from_rust"){
					var update_id = body.id
					var update_type = body.type // "1": premium, "2": discount
					var update_inf = body.inf || "" 
					var update_ref = body.ref || "" 
					var update_digest = body.digest || "" 

					var update_data = body.data 

					var compressed = gzip(new TextEncoder('utf-8').encode(JSON.stringify(update_data)), { to: 'arraybuffer' })

					await env[`premium_dicount_${zoneRegion}_items`].prepare(`
						UPDATE items SET
							"type" = ?1,
							"inf" = ?2,
							"ref" = ?3,
							"digest" = ?4,
							"data" = ?5,
							"updated_at" = ?6
						WHERE "id" = ?7
					`).bind(
						update_type, update_inf, update_ref, update_digest, compressed.buffer, now, update_id
					).run()

					headers.set('Content-Type', 'application/json')
					return new Response(JSON.stringify({ success: true, message: "AI classification complete" }), { headers })
				}

				// -------------------------------------------------------------
				// [분기 2] 사용자가 공유 링크를 제출하는 경우 (최초 등록)
				// -------------------------------------------------------------
				var itemLink = body.link || ""

				if(!itemLink.startsWith('http')){
					headers.set('Content-Type', 'application/json')
					return new Response(JSON.stringify({ success: false, message: "Invalid Link" }), { headers })
				}

				// 명세에 따라 익명 계정 강제 할당
				var fromAddress = ethers.ZeroAddress
				var toAddress = ethers.ZeroAddress

				// ID 생성 (링크 주소와 ZeroAddress의 결합으로 해시 생성)
				var itemId = hashId(itemLink + fromAddress)

				// data 필드: 초기에는 링크만 담아두고, 나머지는 비워둡니다. (Rust가 채울 예정)
				var initialData = {
					head: "",
					body: "",
					link: itemLink
				}

				var compressedData = gzip(new TextEncoder('utf-8').encode(JSON.stringify(initialData)), { to: 'arraybuffer' })

				// 1. items 테이블에 Draft 상태("0")로 저장
				await env[`premium_dicount_${zoneRegion}_items`].prepare(`
					INSERT INTO items (
						"id", "type", "from", "to", "cc", "inf", "ref", "digest", "data", "created_at", "updated_at"
					) VALUES (
						?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11
					) ON CONFLICT (id) DO NOTHING
				`).bind(
					itemId,
					"0",         // type: "0" (Draft 상태)
					fromAddress, // from: ZeroAddress
					toAddress,   // to: ZeroAddress
					cc,
					"", "", "",  // inf, ref, digest 초기화
					compressedData.buffer,
					now,
					now
				).run()

				// 2. Rust가 가져갈 수 있도록 crons(작업 큐) 테이블에 등록
				await env[`premium_dicount_${zoneRegion}_items`].prepare(`
					INSERT INTO crons (
						"id", "cc", "bcc", "job", "ref", "created_at", "updated_at"
					) VALUES (
						?1, ?2, ?3, ?4, ?5, ?6, ?7
					) ON CONFLICT (id) DO NOTHING
				`).bind(
					itemId,
					cc,
					"", // bcc
					null, // job (Rust는 items 테이블의 data 필드를 읽어서 link를 확인할 수 있으므로 비워둠)
					toAddress,
					now,
					now
				).run()

				headers.set('Content-Type', 'application/json')
				return new Response(JSON.stringify({ success: true, id: itemId, status: "draft" }), { headers })

			}else if(request.method == 'GET'){
				var { results } = await env.DB.prepare(`
					SELECT * FROM items 
					WHERE "created_at" < ?1 
					ORDER BY created_at DESC 
					LIMIT 20
				`).bind(now).all();

				// 데이터 압축 해제 처리
				if (results && results.length > 0) {
					for (var i = 0; i < results.length; i++) {
						var item = results[i];
						if (item.data) {
							try {
								var decompressed = new TextDecoder('utf-8').decode(ungzip(item.data));
								item.data = JSON.parse(decompressed);
							} catch (e) {
								console.log('Decompression error for item', item.id);
							}
						}
					}
				}

				if(contentType == 'application/json'){
					headers.set('Content-Type', contentType)


					return new Response(JSON.stringify({
						results:results
					}), {
						headers:headers
					})
				}else{
					headers.set('Content-Type', 'text/html; charset=utf-8')

					return new Response(renderHtml({}, results), {
						headers:headers
					})
				}
			}else if(request.method == 'OPTIONS'){
				return new Response('', {
					headers:headers
				})
			}
		}catch(err){
			console.log('err',err)
		}

		return new Response(`I'm a teapot!`, {
			status:418,
			headers: { "Content-Type": "text/html; charset=utf-8" },
		})
	},
} satisfies ExportedHandler<Env>