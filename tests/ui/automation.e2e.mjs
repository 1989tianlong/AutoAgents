// 在无头 WebKit 里验证自动化规则与决策中心：建规则（Issue 打标签 / 新 PR）→ 自动建工作流并跑、
// 规则的启停与删除、参数校验、PR 审查结论贴回；决策中心聚合四类待办、侧边栏角标、
// 一键合并 PR 完成验收。Tauri 接口由 mock.mjs 模拟。
//
// 用法：npm run dev 之后执行 npm run test:ui（或单独 node tests/ui/automation.e2e.mjs）
import { createRequire } from "node:module";
import { MOCK } from "./mock.mjs";

const [, , urlArg, pwDirArg] = process.argv;
const url = urlArg ?? "http://localhost:1430/#/inbox";
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
const ruleCard = (name) => page.locator("main [data-testid=rule]").filter({ hasText: name }).first();

console.log("\n[0] 决策中心：初始待办与侧边栏角标");
await page.getByRole("heading", { name: "决策中心", exact: true }).waitFor();
// mock 初始数据：任务 #2 在 reviewing（PR #101，检查一项失败）、任务 #1 待启动 → 2 件待办
check("待验收的 PR 列出来", await waitFor(async () => (await page.locator("[data-testid=inbox-review]").count()) === 1));
check("PR 的检查状态归成「检查未过」", (await page.locator("[data-testid=inbox-review]").getByText("检查未过", { exact: true }).count()) === 1);
check("待启动的任务列出来", (await page.locator("[data-testid=inbox-start]").count()) === 1);
check("侧边栏角标是待办总数", (await page.locator("[data-testid=inbox-badge]").textContent()) === "2");

console.log("\n[1] 一键合并 PR：验收完成");
await page.locator("[data-testid=inbox-merge]").click();
check("合并后从待验收里消失", await waitFor(async () => (await page.locator("[data-testid=inbox-review]").count()) === 0));
check("任务标成完成", await page.evaluate(() => window.__mock.state.tasks.find((t) => t.id === 2).status === "done"));
check("角标跟着减一", await waitFor(async () => (await page.locator("[data-testid=inbox-badge]").textContent()) === "1"));

console.log("\n[2] 建一条「Issue 打标签」规则");
await page.goto("http://localhost:1430/#/workflows");
await page.getByRole("heading", { name: "自动化规则", exact: true }).waitFor();
check("空状态给引导", (await page.getByText("还没有自动化规则", { exact: true }).count()) === 1);
await page.getByRole("button", { name: "＋ 建规则" }).click();
await page.getByRole("heading", { name: "新建自动化规则" }).waitFor();
check("两种触发方式都列出来", (await page.getByText("Issue 打标签", { exact: true }).count()) >= 1
  && (await page.getByText("新 PR", { exact: true }).count()) >= 1);
// 先试一下参数校验：没填标签
await page.getByTestId("rule-name").fill("demo 的 bug 自动修");
await page.getByTestId("rule-repo").fill("octo/demo");
await page.getByTestId("rule-create-submit").click();
check("标签没填会拦下来", await waitFor(async () => (await page.locator("[data-testid=rule-error]").count()) === 1));
await page.getByTestId("rule-label").fill("agent-fix");
await page.getByTestId("rule-workdir").fill("/Users/tester/code/demo");
await page.getByTestId("rule-create-submit").click();
await waitFor(async () => (await dialog().count()) === 0);
check("规则卡出现且生效中", await waitFor(async () => (await ruleCard("demo 的 bug 自动修").count()) === 1)
  && (await ruleCard("demo 的 bug 自动修").getByText("生效中", { exact: true }).count()) === 1);
check("规则卡写着标签与流程", (await ruleCard("demo 的 bug 自动修").getByText(/agent-fix/).count()) >= 1
  && (await ruleCard("demo 的 bug 自动修").getByText(/修 Issue 流水线/).count()) >= 1);
// 建完立刻扫了一遍：工作流已经建出来并开跑
check("自动建出了修 Issue 工作流", await waitFor(async () =>
  (await page.locator("[data-testid=workflow]").filter({ hasText: "修 Issue 流水线" }).count()) === 1));
check("触发记录里是那个 Issue", await page.evaluate(() =>
  window.__mock.state.ruleRuns[0]?.key === "issue:9"
  && window.__mock.state.ruleRuns[0]?.workflowId != null));
check("活动流里有规则触发记录", await page.evaluate(() =>
  window.__mock.state.events.some((e) => e.kind === "automation.triggered" && e.message.includes("issue:9"))));

console.log("\n[3] 引擎推进 + 再扫不重复触发");
const wfId = await page.evaluate(() => window.__mock.state.workflows[0].id);
for (let i = 0; i < 4; i++) {
  await page.evaluate((x) => window.__mock.wfTick(x), wfId);
  await sleep(100);
}
check("三步链跑到完成", await waitFor(async () =>
  (await page.locator("[data-testid=workflow]").filter({ hasText: "修 Issue 流水线" }).getByText("已完成", { exact: true }).count()) === 1));
await page.getByRole("button", { name: "立即检查" }).click();
await sleep(200);
check("再扫不重复建工作流", await page.evaluate(() => window.__mock.state.workflows.length === 1));

console.log("\n[4] 建一条「新 PR」规则：审查 + 结论贴回");
await page.getByRole("button", { name: "＋ 建规则" }).click();
await page.getByRole("heading", { name: "新建自动化规则" }).waitFor();
await page.locator('input[name="rule-kind"]').nth(1).check();
await page.getByTestId("rule-name").fill("新 PR 自动审查");
await page.getByTestId("rule-repo").fill("octo/demo");
await page.getByTestId("rule-create-submit").click();
await waitFor(async () => (await dialog().count()) === 0);
check("PR 审查工作流自动开跑", await waitFor(async () =>
  (await page.locator("[data-testid=workflow]").filter({ hasText: "PR 审查流水线" }).count()) === 1));
const reviewWf = await page.evaluate(() =>
  window.__mock.state.workflows.find((w) => w.template === "pr-review").id);
for (let i = 0; i < 4; i++) {
  await page.evaluate((x) => window.__mock.wfTick(x), reviewWf);
  await sleep(100);
}
check("审查三步跑完", await waitFor(async () =>
  (await page.locator("[data-testid=workflow]").filter({ hasText: "PR 审查流水线" }).getByText("已完成", { exact: true }).count()) === 1));
await page.getByRole("button", { name: "立即检查" }).click();
check("结论贴回了 PR（事件）", await waitFor(async () => page.evaluate(() =>
  window.__mock.state.events.some((e) => e.kind === "automation.commented" && e.message.includes("PR #12")))));

console.log("\n[5] 规则的启停与删除");
await ruleCard("新 PR 自动审查").getByRole("button", { name: "停用" }).click();
check("停用后显示「已停用」", await waitFor(async () =>
  (await ruleCard("新 PR 自动审查").getByText("已停用", { exact: true }).count()) === 1));
await ruleCard("新 PR 自动审查").getByRole("button", { name: "启用" }).click();
check("可以重新启用", await waitFor(async () =>
  (await ruleCard("新 PR 自动审查").getByText("生效中", { exact: true }).count()) === 1));
await ruleCard("新 PR 自动审查").getByRole("button", { name: "删除" }).click();
check("删除后只剩一条规则", await waitFor(async () => (await page.locator("[data-testid=rule]").count()) === 1));

console.log("\n[6] 决策中心跟着自动化更新");
await page.goto("http://localhost:1430/#/inbox");
await page.getByRole("heading", { name: "决策中心", exact: true }).waitFor();
// 修 Issue 工作流跑完后它的任务停在「改动待确认」
check("自动修出来的任务在等确认", await waitFor(async () => (await page.locator("[data-testid=inbox-change]").count()) >= 1));

const failed = results.filter((r) => !r).length;
console.log(`\nautomation.e2e: ${results.length - failed}/${results.length} 项通过${failed ? "" : "，全绿"}${pageErrors.length ? `；页面报错 ${pageErrors.length} 条` : ""}`);
if (pageErrors.length) console.log(pageErrors.slice(0, 5).join("\n"));
await browser.close();
process.exit(failed || pageErrors.length ? 1 : 0);
