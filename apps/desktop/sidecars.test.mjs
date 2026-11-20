// 사이드카 준비(sidecars.mjs)와 설정 · 찾는 쪽의 약속.
// 실행: node --test apps/desktop/sidecars.test.mjs  (premium-discount 폴더에서)
//
// - 빌드한 실행 파일을 Tauri 가 찾는 이름(`binaries/<이름>-<삼중항>[.exe]`)으로 옮긴다 (교차 빌드 자리 포함), 없으면 실패
// - tauri.sidecar.conf.json 의 externalBin 이 그 두 이름이고, 빌드 전에 sidecars.mjs 를 부른다. 기본 설정에는 사이드카가 없다
// - 설치할 때 삼중항을 뗀 이름과 설치 폴더(productName)가 premium-discount-mcp 의 find_pd_app · find_mcp_server 가 찾는 것과 같다
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { mkdirSync, mkdtempSync, readFileSync, readdirSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { SIDECARS, exeSuffix, stage, stagedName } from './sidecars.mjs'

const here = dirname(fileURLToPath(import.meta.url))
const readJson = (name) => JSON.parse(readFileSync(join(here, name), 'utf8'))
const quiet = () => {}

// 루트 작업 공간 하나: 두 사이드카 모두 <작업 공간>/target/[삼중항/]release 에 생긴다
function fakeBuilds(target, cross) {
  const root = mkdtempSync(join(tmpdir(), 'pd-sidecars-'))
  const workspaceDir = join(root, 'premium-discount')
  const release = join(workspaceDir, 'target', ...(cross ? [target] : []), 'release')
  mkdirSync(release, { recursive: true })
  for (const name of SIDECARS) {
    writeFileSync(join(release, name + exeSuffix(target)), `${name} for ${target}`)
  }
  return { root, workspaceDir, out: join(root, 'binaries') }
}

test('빌드한 두 실행 파일을 <이름>-<삼중항>[.exe] 로 옮긴다 (Windows · 리눅스 · 교차 빌드)', () => {
  for (const [target, cross] of [
    ['x86_64-pc-windows-msvc', false],
    ['x86_64-unknown-linux-gnu', false],
    ['aarch64-apple-darwin', true],
  ]) {
    const dirs = fakeBuilds(target, cross)
    const built = []
    const staged = stage({ target, cross, ...dirs, build: (...a) => built.push(a), log: quiet })
    assert.deepEqual(
      readdirSync(dirs.out).sort(),
      SIDECARS.map((n) => stagedName(n, target)).sort(),
      target
    )
    assert.equal(staged.length, 2)
    for (const name of SIDECARS) {
      assert.equal(readFileSync(join(dirs.out, stagedName(name, target)), 'utf8'), `${name} for ${target}`)
    }
    // cargo build --release --manifest-path <작업 공간>/Cargo.toml -p <이름> --bin <이름> (교차면 --target)
    assert.deepEqual(
      built.map(([manifest, pkg, bin, extra]) => [manifest, pkg, bin, extra]),
      SIDECARS.map((n) => [join(dirs.workspaceDir, 'Cargo.toml'), n, n, cross ? ['--target', target] : []])
    )
  }
  assert.equal(stagedName('premium-discount-app', 'x86_64-pc-windows-msvc'), 'premium-discount-app-x86_64-pc-windows-msvc.exe')
  assert.equal(stagedName('premium-discount-mcp', 'x86_64-unknown-linux-gnu'), 'premium-discount-mcp-x86_64-unknown-linux-gnu')
})

test('--no-build 인데 실행 파일이 없으면 그 경로를 알리고 멈춘다', () => {
  const dirs = fakeBuilds('x86_64-pc-windows-msvc', false)
  assert.throws(
    () => stage({ target: 'x86_64-unknown-linux-gnu', noBuild: true, ...dirs, log: quiet }),
    /premium-discount-app.*이 없습니다/
  )
})

test('설정: 사이드카 설정의 externalBin 은 두 사이드카, 빌드 전에 sidecars.mjs 를 부르고, 기본 설정에는 없다', () => {
  const side = readJson('tauri.sidecar.conf.json')
  assert.deepEqual(side.bundle.externalBin, SIDECARS.map((n) => `binaries/${n}`))
  assert.match(side.build.beforeBuildCommand, /node desktop\/sidecars\.mjs/)
  const base = readJson('tauri.conf.json')
  assert.equal(base.bundle.externalBin, undefined, 'a window-only build must not need the sidecars')
})

test('설치 이름 · 폴더가 premium-discount-mcp 가 찾는 것과 같다', () => {
  const lib = readFileSync(join(here, '../../crates/premium-discount-mcp/src/lib.rs'), 'utf8')
  const product = lib.match(/pub const PD_PRODUCT_NAME: &str = "([^"]+)";/)[1]
  assert.equal(readJson('tauri.conf.json').productName, product)
  // 설치할 때 Tauri 는 삼중항을 떼어 `<이름>[.exe]` 로 둔다. find_pd_app · find_mcp_server 가 그 이름을 찾는다
  for (const name of SIDECARS) {
    assert.ok(lib.includes(`format!("${name}{}", std::env::consts::EXE_SUFFIX)`), `${name} lookup`)
  }
})
