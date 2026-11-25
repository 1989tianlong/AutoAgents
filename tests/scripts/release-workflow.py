#!/usr/bin/env python3
"""在不联网、不发布的前提下，把 .github/workflows/release.yml 里的每段脚本跑一遍。

用法：python3 tests/scripts/release-workflow.py        （需要 PyYAML：pip install pyyaml）

- 每段 `run:` 脚本先做 bash 语法检查
- 打包脚本：用假的可执行文件跑一遍，核对压缩包内容与校验值
- 汇总校验值的脚本：用四个假的 artifact 跑一遍
- 创建 Release 的脚本：用替身的 gh 跑一遍。替身和真的 gh 一样，不在 git 仓库里
  又没被告知仓库（--repo / -R 或 GH_REPO）就直接报错

发布流程平时不跑，一跑就是十几分钟；改了它先在这里过一遍。
"""
import hashlib
import os
import pathlib
import shutil
import subprocess
import sys
import tarfile
import tempfile

import yaml

TARGETS = [
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
]
STUB_GH = """#!/bin/sh
# 替身的 gh：只把参数记下来。`release view` 当作「还没有这个 Release」。
# 和真的 gh 一样：不在 git 仓库里的时候，既没给 --repo / -R 也没有 GH_REPO，就直接报错
case " $* " in
  *" --repo "*|*" -R "*) ;;
  *) [ -n "${GH_REPO:-}" ] || { echo "failed to run git: fatal: not a git repository (or any of the parent directories): .git" >&2; exit 1; } ;;
esac
printf '%s\\n' "$*" >> "$GH_ARGS_LOG"
case "$1 $2" in
  "release view") exit 1 ;;
esac
exit 0
"""


def sha256(path):
    return hashlib.sha256(pathlib.Path(path).read_bytes()).hexdigest()



def step_env(workflow, job_name, step_name):
    """某个步骤在 `env:` 里声明的变量（把 ${{ … }} 换成测试值）"""
    values = {
        "${{ github.token }}": "TOKEN",
        "${{ github.ref_name }}": "v0.2.0",
        "${{ github.repository }}": "1989tianlong/AutoAgents",
    }
    for job in workflow["jobs"].values():
        if job["name"] == job_name:
            for step in job["steps"]:
                if step.get("name") == step_name:
                    return {key: values.get(value, value) for key, value in step.get("env", {}).items()}
    return {}


def main():
    repo = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[2]
    workflow = yaml.safe_load((repo / ".github/workflows/release.yml").read_text())
    replace = {
        "${{ matrix.target }}": TARGETS[0],
        "${{ matrix.zig }}": "true",
        "${{ github.token }}": "TOKEN",
        "${{ github.ref_name }}": "v0.2.0",
    }

    def render(script):
        for key, value in replace.items():
            script = script.replace(key, value)
        return script

    scripts = []
    for job in workflow["jobs"].values():
        for step in job["steps"]:
            if "run" in step:
                scripts.append((job["name"], step.get("name", "?"), render(step["run"])))

    ok = True
    sandbox = pathlib.Path(tempfile.mkdtemp(prefix="autoagents-relcheck-"))
    try:
        # 1. 语法
        for job, step, script in scripts:
            path = sandbox / "script.sh"
            path.write_text(script)
            done = subprocess.run(["bash", "-n", str(path)], capture_output=True, text=True)
            print(f"syntax  [{'ok' if done.returncode == 0 else 'FAIL'}] {job} · {step} {done.stderr.strip()}")
            ok &= done.returncode == 0

        def run(script, cwd, env_extra, label):
            nonlocal ok
            path = sandbox / "script.sh"
            path.write_text(script)
            env = dict(os.environ)
            env.pop("GH_TOKEN", None)
            env.update(env_extra)
            done = subprocess.run(["bash", str(path)], cwd=cwd, env=env, capture_output=True, text=True)
            print(f"run     [{'ok' if done.returncode == 0 else 'FAIL'}] {label}")
            if done.returncode != 0:
                print(done.stdout[-1500:], done.stderr[-1500:])
            ok &= done.returncode == 0
            return done

        by_step = {step: script for job, step, script in scripts}

        # 2. 打包：假装已经构建出两个可执行文件
        pkg = sandbox / "pkg"
        release = pkg / "target" / TARGETS[0] / "release"
        release.mkdir(parents=True)
        (release / "agentd").write_text("dummy agentd\n")
        (release / "autoagents-relay").write_text("dummy relay\n")
        archive = f"autoagents-node-{TARGETS[0]}.tar.gz"
        packaging = next(s for j, st, s in scripts if st == "打包并生成校验值")
        run(packaging, pkg, {"BINARIES": "agentd autoagents-relay"}, "打包并生成校验值（假的可执行文件）")
        packed = pkg / "dist"
        members = tarfile.open(packed / archive).getnames()
        want = sha256(packed / archive)
        got = (packed / "SHA256SUMS").read_text().split()[0]
        good = sorted(members) == ["agentd", "autoagents-relay"] and want == got
        print(f"check  [{'ok' if good else 'FAIL'}] 压缩包里是两个程序，校验值对得上：{members}")
        ok &= good

        # 3. 发布：四个 artifact 目录 → 汇总校验 → 替身 gh 创建 Release
        rel = sandbox / "rel"
        for i, target in enumerate(TARGETS):
            art = rel / "dist" / f"binaries-{i}"
            art.mkdir(parents=True)
            name = f"autoagents-node-{target}.tar.gz"
            (art / name).write_text(f"dummy build for {target}\n")
            (art / "SHA256SUMS").write_text(f"{sha256(art / name)}  {name}\n")
        merge = next(s for j, st, s in scripts if st == "汇总校验值，逐个核对")
        run(merge, rel, {}, "汇总校验值，逐个核对（四个 artifact）")
        sums = [line.split()[1] for line in (rel / "SHA256SUMS").read_text().splitlines()]
        good = sorted(sums) == sorted(f"autoagents-node-{t}.tar.gz" for t in TARGETS) and sums == sorted(sums)
        print(f"check  [{'ok' if good else 'FAIL'}] SHA256SUMS 四行、按文件名排：{sums}")
        ok &= good

        stub_bin = sandbox / "bin"
        stub_bin.mkdir()
        (stub_bin / "gh").write_text(STUB_GH)
        (stub_bin / "gh").chmod(0o755)
        args_log = sandbox / "gh-args.log"
        publish = next(s for j, st, s in scripts if st == "创建 Release 并上传")
        env_extra = {"PATH": f"{stub_bin}:{os.environ['PATH']}", "GH_TOKEN": "TOKEN", "TAG": "v0.2.0", "GH_ARGS_LOG": str(args_log)}
        env_extra.update(step_env(workflow, "发布", "创建 Release 并上传"))
        run(publish, rel, env_extra, "创建 Release 并上传（替身 gh）")
        args = args_log.read_text()
        print("------ 替身 gh 收到的参数 ------")
        print(args[:2500])
        print("-------------------------------")
        good = "release create v0.2.0" in args and "--generate-notes" in args and "\n\n| 平台 | 文件 |" in args
        print(f"check  [{'ok' if good else 'FAIL'}] 创建 Release：带自动变更日志，说明里的表格不带缩进")
        ok &= good
    finally:
        shutil.rmtree(sandbox, ignore_errors=True)
    print("\nRESULT:", "all good" if ok else "PROBLEMS FOUND")
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
