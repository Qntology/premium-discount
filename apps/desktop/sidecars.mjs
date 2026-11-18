// 데스크톱 설치본에 넣을 사이드카 두 개를 만들어 Tauri 가 찾는 이름으로 둔다.
//
//   premium-discount-app  헤드리스 코어 (serve_lines, ../core = apps/core)
//   premium-discount-mcp  Raon Zena 가 부르는 MCP 서버 (../../crates/premium-discount-mcp)
// 둘 다 루트 작업 공간(../../Cargo.toml)의 멤버라 한 번에 빌드되고, 산출물은 루트 target/ 에 있다(.cargo/config.toml).
//
// Tauri 의 bundle.externalBin 은 빌드할 때 `binaries/<이름>-<대상 삼중항>[.exe]` 를 찾고, 설치할 때 삼중항을 떼어
// 창 실행 파일 옆에 둔다(%LOCALAPPDATA%\premium.discount\premium-discount-app.exe …). 그래서 raon-zena 의
// premium-discount-mcp 는 설치 폴더에서 코어를 찾는다(find_pd_app · find_mcp_server, crates/premium-discount-mcp/src/lib.rs).
//
// 쓰는 곳: tauri.sidecar.conf.json 의 beforeBuildCommand (apps/desktop 에서 `cargo tauri build --config tauri.sidecar.conf.json`).
// Tauri 는 앞 명령을 화면 폴더에서 돌리는데, apps/desktop 아래에 package.json 이 없으면 그 폴더는 tauri.conf.json 폴더의 부모(apps/)다.
// 그래서 설정의 명령은 apps/ 기준 경로(../web/desktop, desktop/sidecars.mjs)로 적는다.
// 사이드카는 기본 설정(tauri.conf.json)에 넣지 않는다: tauri-build 는 externalBin 파일이 없으면 `cargo build` 자체를
// 멈추므로, 사이드카 없이 창만 고치는 개발 빌드가 깨진다.
//
// 사용: node apps/desktop/sidecars.mjs [--no-build] [--target <삼중항>]   (어느 폴더에서 불러도 같다: 경로는 이 파일 기준)
//   --no-build  이미 만든 release 실행 파일을 옮기기만 한다
//   --target    교차 빌드 대상 (기본: rustc 의 host)
//
// 이름 규칙과 설정의 약속은 sidecars.test.mjs 가 본다 (`node --test apps/desktop/sidecars.test.mjs`).

import { execFileSync } from 'node:child_process'
import { copyFileSync, existsSync, mkdirSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))

/** 사이드카 이름 (tauri.sidecar.conf.json 의 externalBin 은 `binaries/<이름>`) */
export const SIDECARS = ['premium-discount-app', 'premium-discount-mcp']

/** rustc 의 host 삼중항 */
export function hostTriple() {
  return execFileSync('rustc', ['-vV'], { encoding: 'utf8' })
    .split('\n')
    .find((l) => l.startsWith('host:'))
    .slice(5)
    .trim()
}

/** 대상의 실행 파일 확장자 */
export function exeSuffix(target) {
  return target.includes('windows') ? '.exe' : ''
}

/** Tauri 가 빌드할 때 찾는 파일 이름: `<이름>-<삼중항>[.exe]` (설치할 때 삼중항을 뗀다) */
export function stagedName(name, target) {
  return `${name}-${target}${exeSuffix(target)}`
}

/**
 * 사이드카를 만들고(build) `out` 에 Tauri 이름으로 복사한다. 복사한 경로들을 돌려준다.
 * 두 사이드카 모두 루트 작업 공간(`workspaceDir`/Cargo.toml)에서 빌드하고 `workspaceDir`/target 에서 찾는다.
 * cross 가 true 면 `cargo build --target` 의 자리(target/<삼중항>/release)에서 찾는다.
 */
export function stage({
  target,
  cross = false,
  noBuild = false,
  workspaceDir = resolve(here, '../..'),
  out = join(here, 'binaries'),
  build = (manifest, pkg, bin, extra) =>
    execFileSync('cargo', ['build', '--release', '--manifest-path', manifest, '-p', pkg, '--bin', bin, ...extra], { stdio: 'inherit' }),
  log = (m) => console.log(m),
}) {
  const manifest = join(workspaceDir, 'Cargo.toml')
  mkdirSync(out, { recursive: true })
  const staged = []
  for (const name of SIDECARS) {
    const extra = cross ? ['--target', target] : []
    if (!noBuild) {
      log(`[sidecars] cargo build --release -p ${name} --bin ${name} ${extra.join(' ')}`)
      build(manifest, name, name, extra)
    }
    const built = join(workspaceDir, 'target', ...(cross ? [target] : []), 'release', name + exeSuffix(target))
    if (!existsSync(built)) {
      throw new Error(`[sidecars] ${built} 이 없습니다 (--no-build 이면 먼저 cargo build --release 를 하세요)`)
    }
    const dest = join(out, stagedName(name, target))
    copyFileSync(built, dest)
    log(`[sidecars] ${built} -> ${dest}`)
    staged.push(dest)
  }
  return staged
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const args = process.argv.slice(2)
  const targetFlag = args.indexOf('--target')
  const cross = targetFlag >= 0
  try {
    stage({ target: cross ? args[targetFlag + 1] : hostTriple(), cross, noBuild: args.includes('--no-build') })
  } catch (e) {
    console.error(e.message)
    process.exit(1)
  }
}
