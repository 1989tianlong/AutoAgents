// 在无头 WebKit 里验证任务页：从 Issue 或一段需求出发，启动、看改动、确认提交、跟踪 PR 与检查，
// 失败时重试、停止、删除，以及 GitHub 页转任务、会话页链回任务。Tauri 接口由 mock.mjs 模拟。
//
// 用法：npm run dev 之后执行 npm run test:ui（或单独 node tests/ui/tasks.e2e.mjs）
import { createRequire } from "node:module";
import { MOCK } from "./mock.mjs";

const [, , urlArg, pwDirArg] = process.argv;
const url = urlArg ?? "http://localhost:1430/#/tasks";
const require = createRequire(import.meta.url);
const pwDir = pwDirArg ?? process.env.PLAYWRIGHT_CORE ?? "playwright-core";
const { webkit } = require(pwDir);

const results = [];
const check = (name, cond, extra = "") => {
  results.push(!!cond);
  console.log(`  ${cond ? "✓" : "✗"} ${name}${cond || !extra ? "" : `  ← ${extra}`}`);
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const waitFor = async (fn, ms = 8000) => {
  const t0 = Date.now();
  while (Date.now() - t0 < ms) {
    if (await fn()) return true;
    await sleep(80);
  }
  return false;
};

const browser = await webkit.launch();
const page = await browser.newPage({ viewport: { width: 1440, height: 960 } });
const pageErrors = [];
page.on("pageerror", (e) => pageErrors.push(String(e)));
page.on("console", (m) => { if (m.type() === "error") pageErrors.push(m.text()); });
page.on("dialog", (dialog) => dialog.accept());
await page.addInitScript(MOCK);
await page.goto(url);

const lastCall = (cmd) => page.evaluate((c) => window.__mock.lastCall(c), cmd);
/** 详情区里的状态标签 */
const statusBadge = (text) => page.locator("section span", { hasText: new RegExp(`^${text}$`) }).first();
/** 左边清单里的一个任务 */
const item = (title) => page.locator("aside button", { hasText: title }).first();
const chip = (label) => page.locator("aside button", { hasText: new RegExp(`^${label}`) }).first();
const dialog = () => page.locator("div.fixed.inset-0");

console.log("\n[0] 清单与筛选");
await page.getByRole("heading", { name: "任务", exact: true }).waitFor();
await item("修复登录态过期后白屏").waitFor();
check("按状态归类计数", await chip("待启动").innerText().then((t) => t.includes("待启动") && t.includes("1")));
check("三类里各有一个", (await chip("审查中").innerText()).includes("1") && (await chip("失败").innerText()).includes("1"));
await chip("待启动").click();
check("筛选后只剩这一类", (await item("给部署页补测试").count()) === 0 && (await item("修复登录态过期后白屏").count()) === 1);
await item("修复登录态过期后白屏").click();
await statusBadge("待启动").waitFor();
check("还没启动的任务给出下一步", await page.getByRole("button", { name: "▶ 启动" }).isVisible());
check("标出来源是 Issue", await page.locator("section span", { hasText: /^Issue$/ }).first().isVisible());
check("链回 Issue", await page.getByRole("button", { name: "Issue #7 ↗" }).isVisible());

console.log("\n[1] 新建任务：直接写一段需求");
await chip("全部").click();
await page.getByRole("button", { name: "＋ 新建" }).click();
await page.getByRole("heading", { name: "新建任务" }).waitFor();
await page.getByPlaceholder("一句话说清楚要做什么").fill("给首页加缓存");
await page.locator("select").selectOption("octo/demo");
await page.getByPlaceholder(/这段话会原样交给 Agent/).fill("首页首屏加载要 5 秒。加上缓存，目标是 1 秒内。");
await page.getByRole("button", { name: "创建", exact: true }).click();
await page.getByRole("heading", { name: "给首页加缓存" }).waitFor();
const created = await lastCall("task_create_issue");
const manual = await lastCall("task_create_manual");
check("task_create_manual 收到标题、仓库、说明", manual?.args.title === "给首页加缓存" && manual?.args.repo === "octo/demo" && manual.args.instructions.includes("缓存"), JSON.stringify(manual?.args));
check("创建后直接打开它", page.url().includes("t=4") && (await statusBadge("待启动").isVisible()));
check("标出来源是手动", await page.locator("section span", { hasText: /^手动$/ }).first().isVisible());

console.log("\n[2] 启动：选机器、Agent、仓库位置");
await page.getByRole("button", { name: "▶ 启动" }).click();
await page.getByRole("heading", { name: "启动任务" }).waitFor();
check("默认在本机、用第一个能干活的 Agent", (await page.locator("button.border-sky-600", { hasText: "本机" }).count()) >= 1 && (await page.locator("button.border-sky-600", { hasText: "Claude CLI" }).first().isVisible()));
check("仓库位置默认在那台机器的代码目录下", (await page.getByPlaceholder(/Users\/tester\/code\/demo/).inputValue()) === "/Users/tester/code/demo");
check("默认要人工确认", (await page.locator("input[type=checkbox]").isChecked()) === false);
await page.getByPlaceholder(/Users\/tester\/code\/demo/).fill("/tmp/not-a-repo");
await dialog().getByRole("button", { name: "启动", exact: true }).click();
check("目录不对时当场显示原因", await waitFor(() => page.getByText(/确认仓库位置失败/).isVisible()));
await page.getByPlaceholder(/Users\/tester\/code\/demo/).fill("/Users/tester/code/demo");
await page.getByRole("button", { name: /用 SSH 克隆到这个位置/ }).click();
const clone = await lastCall("repo_clone");
check("机器上没有仓库时可以用 SSH 克隆", clone?.args.nodeId === "local" && clone.args.url === "git@github.com:octo/demo.git" && clone.args.dir === "/Users/tester/code/demo", JSON.stringify(clone?.args));
await dialog().getByRole("button", { name: "启动", exact: true }).click();
await waitFor(async () => (await dialog().count()) === 0);
const start = await lastCall("task_start");
check("task_start 带上机器、Agent、仓库位置", start?.args.id === 4 && start.args.nodeId === "local" && start.args.adapter === "claude-cli" && start.args.workdir === "/Users/tester/code/demo" && start.args.baseBranch === null && start.args.autoPr === false, JSON.stringify(start?.args));
await statusBadge("进行中").waitFor();
check("进行中：可以停止、可以去会话旁听", await page.getByRole("button", { name: "停止这一轮" }).isVisible() && await page.getByRole("button", { name: "查看会话" }).isVisible());
check("工作树是独立的目录", await page.getByText("/Users/tester/code/demo-wt/agent-task-4").first().isVisible());

// Agent 做完了：页面每 5 秒对一次状态
await page.evaluate(() => window.__mock.agentDone(4));
check("做完后进入「待确认」", await waitFor(() => statusBadge("待确认").isVisible(), 9000));
check("列出了改动的文件", await waitFor(() => page.locator("pre", { hasText: "+new line" }).isVisible()));
check("说明有几个文件等你确认", await page.getByText("2 个文件有改动，等你确认").isVisible());
await page.getByText("src/app/new.ts", { exact: true }).click();
check("点另一个文件看它的改动", await waitFor(() => page.locator("pre", { hasText: "+export const fresh = 1;" }).isVisible()));
const patch = await lastCall("task_diff_file");
check("未跟踪的文件按整份新增读", patch?.args.path === "src/app/new.ts" && patch.args.untracked === true, JSON.stringify(patch?.args));
check("还没提交：分支上没有多出来的提交", (await page.getByText("已提交", { exact: false }).count()) === 0);

console.log("\n[3] 确认之后提交、开 PR；跟踪检查");
await page.getByRole("button", { name: "提交并创建 PR" }).click();
await page.getByRole("heading", { name: "提交并创建 PR" }).waitFor();
check("提交前说清楚会往外发什么", await page.getByText(/向 .* 开一个 PR/).isVisible());
await dialog().getByRole("button", { name: "确认提交" }).click();
await statusBadge("审查中").waitFor();
const submit = await lastCall("task_submit");
check("task_submit 带任务 id", submit?.args.id === 4, JSON.stringify(submit?.args));
check("显示 PR 链接", await page.getByRole("button", { name: "PR #104 ↗" }).isVisible());
check("逐项列出检查", await page.getByText("build", { exact: true }).isVisible() && await page.getByText("lint", { exact: true }).isVisible());
check("失败的检查标出来", await page.getByText("失败", { exact: true }).count() >= 1 && (await page.getByText("通过", { exact: true }).count()) >= 1);
await page.getByRole("button", { name: "让 Agent 修复" }).click();
check("让 Agent 修：task_fix", (await lastCall("task_fix"))?.args.id === 4);
await statusBadge("进行中").waitFor();

// 检查通过、PR 合并之后
await page.evaluate(() => {
  const task = window.__mock.state.tasks.find((t) => t.id === 4);
  task.status = "reviewing";
  window.__mock.state.mergePr = true;
  window.__mock.emitNode({ id: "local", name: "本机", online: true, message: null });
});
await statusBadge("审查中").waitFor();
await page.getByRole("button", { name: "刷新检查" }).click();
check("合并后标为已完成", await waitFor(() => statusBadge("已完成").isVisible()));
check("说明 PR 已合并", await page.getByText("PR #104 已合并").isVisible());

console.log("\n[4] 失败的任务：原因留在页面上，可以重试");
await item("升级依赖").click();
await page.getByRole("heading", { name: "升级依赖" }).waitFor();
check("显示失败原因", await page.getByText(/这一轮没有完成：这台机器上同时运行的对话已达上限/).isVisible());
check("可以重试", await page.getByRole("button", { name: "重试" }).isVisible());

console.log("\n[5] 中途停止");
await item("修复登录态过期后白屏").click();
await page.getByRole("button", { name: "▶ 启动" }).click();
await page.getByRole("heading", { name: "启动任务" }).waitFor();
await dialog().getByRole("button", { name: "启动", exact: true }).click();
await statusBadge("进行中").waitFor();
await page.getByRole("button", { name: "停止这一轮" }).click();
check("停止：task_abort", (await lastCall("task_abort"))?.args.id === 1);
check("停止后是失败，原因写明已停止", await waitFor(() => statusBadge("失败").isVisible()) && (await page.getByText(/已停止/).first().isVisible()));

console.log("\n[6] 删除任务");
const before = await page.evaluate(() => window.__mock.state.tasks.length);
await page.getByRole("button", { name: "删除", exact: true }).click();
check("task_delete 带任务 id", (await lastCall("task_delete"))?.args.id === 1);
check("清单里少了一个", await waitFor(async () => (await page.evaluate(() => window.__mock.state.tasks.length)) === before - 1));
check("回到未选择的状态", await waitFor(() => page.getByText("选一个任务", { exact: true }).isVisible()));

console.log("\n[7] GitHub 页：把 Issue 转成任务");
await page.evaluate(() => { window.__mock.state.ghUser = { login: "octo", name: "Octo Cat", avatarUrl: null }; });
await page.evaluate(() => { location.hash = "#/github"; });
await page.getByRole("heading", { name: "GitHub", exact: true }).waitFor();
// 新建任务时选过 octo/demo，GitHub 页记住了这个仓库，直接列它的 Issue
if (await page.getByRole("button", { name: "← 换仓库" }).count() === 0) {
  await page.getByRole("button", { name: /octo\/demo/ }).first().click();
}
await page.getByRole("button", { name: "转任务" }).first().click();
const issue = await lastCall("task_create_issue");
check("task_create_issue 带仓库、Issue 号与链接", issue?.args.repo === "octo/demo" && issue.args.issueNumber === 9 && issue.args.issueUrl.endsWith("/issues/9"), JSON.stringify(issue?.args));
check("提示已创建", await waitFor(() => page.getByText(/已创建任务「首页加载太慢」/).isVisible()));

console.log("\n[8] 会话页标出它属于哪个任务，并能跳回去");
// 流水线为任务 #4 开的那个会话：编号由模拟后端分配，这里问它
const taskSession = await page.evaluate(() => window.__mock.state.sessions.find((s) => s.taskId === 4)?.id);
await page.evaluate((id) => { location.hash = `#/sessions?s=${id}`; }, taskSession);
await page.getByRole("button", { name: "任务 #4 →" }).waitFor();
check("会话头部链回任务", await page.getByRole("button", { name: "任务 #4 →" }).isVisible());
await page.getByRole("button", { name: "任务 #4 →" }).click();
await page.getByRole("heading", { name: "给首页加缓存" }).waitFor();
check("点一下回到任务", page.url().includes("t=4"));

const failed = results.filter((r) => !r).length;
console.log(`\n${results.length - failed}/${results.length} 项通过${pageErrors.length ? `；页面错误 ${pageErrors.length} 条：` : ""}`);
for (const e of pageErrors.slice(0, 8)) console.log("  [page] " + e.slice(0, 300));
await browser.close();
process.exit(failed || pageErrors.length ? 1 : 0);
