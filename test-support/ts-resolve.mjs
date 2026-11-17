// node --import ./test-support/ts-resolve.mjs 로 테스트를 돌리면, 확장자 없는 상대 경로 import('./moderation')를 .ts로 찾는다.
// Workers 번들러(esbuild)는 확장자 없이 import하므로 소스는 그대로 두고, 테스트 실행 쪽에서만 맞춘다.
import { register } from 'node:module'

register('./ts-resolve-hooks.mjs', import.meta.url)
