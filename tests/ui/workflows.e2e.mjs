// 在无头 WebKit 里验证工作流页：模板建流（竞争模式 / 修 Issue / PR 审查）、启动与推进、
// 人工评审择优、失败与重跑、取消，以及发布守护模板链去部署页、活动流里的工作流记录。
// Tauri 接口由 mock.mjs 模拟（引擎用 wfTick 一步一步推）。
//
// 用法：npm run dev 之后执行 npm run test:ui（或单独 node tests/ui/workflows.e2e.mjs）
import { createRequire } from "node:module";
import { MOCK } from "./mock.mjs";

const [, , urlArg, pwDirArg] = process.argv;
const url = urlArg ?? "http://localhost:1430/#/workflows";
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

const dialog = () => page.locator("div.fixed.inset-0");
const cards = () => page.locator("main div.rounded-xl");
const wfCard = (name) => page.locator("main [data-testid=workflow]").filter({ hasText: name }).first();
const stepChips = () => wfCard("·").locator("[data-testid=step]");
const has = (text) => new RegExp(text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"));
/** 引擎推进一步，再等页面跟上（页面每隔几秒自动刷新） */
const tick = async (id, times = 1) => {
  for (let i = 0; i < times; i++) {
    await page.evaluate((x) => window.__mock.wfTick(x), id);
    await sleep(120);
  }
  await waitFor(async () => (await wfCard("·").count()) > 0);
};
const wfId = () => page.evaluate(() => window.__mock.state.workflows[0]?.id);

console.log("\n[0] 模板与空状态");
await page.getByRole("heading", { name: "工作流", exact: true }).waitFor();
check("四个模板都列出来", (await page.getByText("修 Issue 流水线", { exact: true }).count()) === 1
  && (await page.getByText("PR 审查流水线", { exact: true }).count()) === 1
  && (await page.getByText("竞争模式", { exact: true }).count()) === 1
  && (await page.getByText("发布守护流水线", { exact: true }).count()) === 1);
check("发布守护链去部署页", (await page.getByText("已上线 · 去部署页 →", { exact: true }).count()) === 1);
check("可以用的模板带「用这个模板」", (await page.getByRole("button", { name: "用这个模板" }).count()) === 3);
check("空状态给引导", await waitFor(async () => (await page.getByText("还没有工作流", { exact: true }).count()) === 1));

console.log("\n[1] 从竞争模式模板建一个工作流");
// 竞争模式的卡片上点「用这个模板」
const raceCard = page.locator("main div.rounded-xl").filter({ hasText: "竞争模式" }).first();
await raceCard.getByRole("button", { name: "用这个模板" }).click();
await page.getByRole("heading", { name: "新建工作流" }).waitFor();
check("模板默认选中竞争模式", await page.locator('input[name="wf-template"]').nth(2).isChecked());
await page.getByTestId("wf-repo").fill("octo/demo");
await page.getByTestId("wf-issue").fill("7");
await page.getByTestId("wf-workdir").fill("/Users/tester/code/demo");
check("每步的 Agent 按模板建议填好", await page.evaluate(() => {
  const sel = document.querySelectorAll("main select");
  return [...sel].some((s) => s.value === "claude-cli");
}));
// 第一步单独指定模型（另两步留空，沿用全局）
await page.getByTestId("step-model-方案 A").fill("opus-plan");
await dialog().getByRole("button", { name: "创建" }).click();
await waitFor(async () => (await dialog().count()) === 0);
check("卡片出现：待启动", (await wfCard("竞争模式").count()) === 1
  && (await wfCard("竞争模式").getByText("待启动", { exact: true }).count()) === 1);
check("三个步骤都待跑", (await stepChips().count()) === 3);
const createArgs = await lastCallOf("workflow_create");
check("建流的参数带上了每步的 Agent", createArgs?.template === "race"
  && createArgs?.repo === "octo/demo" && createArgs?.issueNumber === 7
  && createArgs?.adapters?.[0] === "claude-cli" && createArgs?.adapters?.[1] === "codex-cli");
check("建流的参数带上了每步单独指定的模型", createArgs?.models?.[0] === "opus-plan"
  && createArgs?.models?.[1] === "" && createArgs?.models?.[2] === "");
check("步骤落在它单独指定的模型上", await waitFor(() => page.evaluate(() => {
  const wf = window.__mock.state.workflows.find((w) => w.template === "race");
  return wf?.steps?.[0]?.model === "opus-plan" && wf?.steps?.[1]?.model == null;
})));

console.log("\n[2] 启动并推进：并行两路，评审门等人");
await wfCard("竞争模式").getByRole("button", { name: "启动" }).click();
await waitFor(async () => (await wfCard("竞争模式").getByText("进行中", { exact: true }).count()) > 0);
const id = await wfId();
await tick(id, 1); // 两路任务步骤跑起来（任务建出来）
await tick(id, 1); // 两路出结果，评审门打开
await waitFor(async () => (await wfCard("竞争模式").getByText("等你决策", { exact: true }).count()) === 1);
check("工作流标成「等你决策」", true);
check("两路都完成", (await stepChips().filter({ hasText: "完成" }).count()) === 2);
check("评审门等你", (await stepChips().filter({ hasText: "人工评审" }).getByText("等你", { exact: true }).count()) === 1);
const taskCount = await page.evaluate(() => window.__mock.state.tasks.length);
check("两路各建了一个任务（各自分支）", taskCount === 5, `tasks=${taskCount}`);
const branches = await page.evaluate(() =>
  window.__mock.state.tasks.filter((t) => t.branch?.includes("-w")).map((t) => t.branch));
check("分支带步骤号，互不冲突", branches.includes("agent/issue-7-w1") && branches.includes("agent/issue-7-w2"), branches.join(","));
check("评审门给出候选按钮", (await page.getByRole("button", { name: /选定「方案 A」（任务 #/ }).count()) === 1);
const gateStepId = await page.evaluate((x) =>
  window.__mock.state.workflows.find((w) => w.id === x).steps.find((s) => s.kind === "gate").id, id);
const chosenTask = await page.evaluate((x) =>
  window.__mock.state.workflows.find((w) => w.id === x).steps.find((s) => s.seq === 1).taskId, id);

console.log("\n[3] 人工评审择优后完成");
await page.getByRole("button", { name: /选定「方案 A」/ }).click();
await waitFor(async () => (await wfCard("竞争模式").getByText("已完成", { exact: true }).count()) === 1);
check("工作流完成", true);
const gateArgs = await lastCallOf("workflow_gate");
check("评审参数是选中的那一路", gateArgs?.stepId === gateStepId && gateArgs?.chosenTaskId === chosenTask);
check("步骤明细里指向任务页", (await wfCard("竞争模式").getByText("去任务页确认改动 →").count()) === 2);

console.log("\n[4] 修 Issue 流水线：失败、级联跳过与重跑");
await page.locator("main div.rounded-xl").filter({ hasText: "修 Issue 流水线" }).first()
  .getByRole("button", { name: "用这个模板" }).click();
await page.getByRole("heading", { name: "新建工作流" }).waitFor();
await page.getByTestId("wf-repo").fill("octo/demo");
await page.getByTestId("wf-instructions").fill("把首页的加载时间降下来");
await page.getByTestId("wf-workdir").fill("/Users/tester/code/demo");
await dialog().getByRole("button", { name: "创建" }).click();
await waitFor(async () => (await dialog().count()) === 0);
const fixCard = wfCard("修 Issue 流水线");
await fixCard.getByRole("button", { name: "启动" }).click();
const fixId = await wfId();
await page.evaluate((x) => window.__mock.wfFail(x, 1), fixId); // 第一步失败
await tick(fixId, 1);
await tick(fixId, 1);
await waitFor(async () => (await fixCard.getByText("失败", { exact: true }).count()) >= 1);
check("工作流标成失败", (await fixCard.getByText("失败", { exact: true }).count()) >= 1);
check("上游失败的两步级联跳过", (await fixCard.getByText("跳过", { exact: true }).count()) === 2
  && (await fixCard.getByText("上游失败，跳过").count()) === 2);
check("失败原因写在步骤上", (await fixCard.getByText("Agent 没有返回任何内容").count()) >= 1);
await fixCard.getByRole("button", { name: "重跑失败步骤" }).click();
await waitFor(async () => (await fixCard.getByText("进行中", { exact: true }).count()) === 1);
check("重跑回到进行中", true);
await tick(fixId, 1);
await tick(fixId, 1);
await tick(fixId, 1);
await tick(fixId, 2);
await waitFor(async () => (await fixCard.getByText("已完成", { exact: true }).count()) === 1);
check("重跑后走完：审查与说明的结论都在", (await fixCard.getByText("看结论", { exact: true }).count()) === 2);

console.log("\n[5] 取消");
await page.locator("main div.rounded-xl").filter({ hasText: "PR 审查流水线" }).first()
  .getByRole("button", { name: "用这个模板" }).click();
await page.getByRole("heading", { name: "新建工作流" }).waitFor();
await page.getByTestId("wf-repo").fill("octo/demo");
await page.getByTestId("wf-pr").fill("101");
await page.getByTestId("wf-workdir").fill("/Users/tester/code/demo");
await dialog().getByRole("button", { name: "创建" }).click();
await waitFor(async () => (await dialog().count()) === 0);
const prCard = wfCard("PR 审查流水线");
await prCard.getByRole("button", { name: "启动" }).click();
const prId = await wfId();
await prCard.getByRole("button", { name: "取消" }).click();
await waitFor(async () => (await prCard.getByText("已取消", { exact: true }).count()) === 1);
check("工作流取消", true);
check("没跑完的步骤标成跳过", (await prCard.getByText("跳过", { exact: true }).count()) === 3);

console.log("\n[6] 活动流里有工作流的记录");
await page.goto("http://localhost:1430/#/activity");
await page.getByRole("heading", { name: "活动", exact: true }).waitFor();
await waitFor(async () => (await page.getByText("建工作流", { exact: true }).count()) >= 1);
check("建流与完成的记录都在", (await page.getByText("全部步骤完成", { exact: true }).count()) >= 1);
const group = page.getByRole("button", { name: "工作流" });
check("按「工作流」筛选的分组在", (await group.count()) === 1);
if ((await group.count()) === 1) {
  await group.click();
  await sleep(200);
  check("筛选后只剩工作流的记录", (await page.getByText("同步状态", { exact: true }).count()) === 0
    && (await page.getByText("建工作流", { exact: true }).count()) >= 1);
}

console.log("\n[7] 建流的校验");
await page.goto("http://localhost:1430/#/workflows");
await page.getByRole("heading", { name: "工作流", exact: true }).waitFor();
await page.locator("main div.rounded-xl").filter({ hasText: "PR 审查流水线" }).first()
  .getByRole("button", { name: "用这个模板" }).click();
await page.getByRole("heading", { name: "新建工作流" }).waitFor();
await page.getByTestId("wf-repo").fill("octo/demo");
await page.getByTestId("wf-workdir").fill("/Users/tester/code/demo");
await dialog().getByRole("button", { name: "创建" }).click();
check("PR 模板没给 PR 号会被拦下", await waitFor(async () => (await page.getByText("请填 PR 号").count()) === 1));

// ---------------------------------------------------------------------------
check("页面没有报错", pageErrors.length === 0, pageErrors.slice(0, 3).join(" | "));
await browser.close();

const passed = results.filter(Boolean).length;
console.log(`\n${passed}/${results.length} 项通过`);
if (passed !== results.length) process.exit(1);

async function lastCallOf(cmd) {
  return page.evaluate((c) => window.__mock.lastCall(c)?.args ?? null, cmd);
}
