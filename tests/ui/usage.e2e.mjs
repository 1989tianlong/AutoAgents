// 在无头 WebKit 里验证用量页：汇总数字、每日预算（设置 → 进度条 → 超了的状态）、
// 按仓库 / 规则 / Agent / 机器的汇总表、趋势与最贵会话。Tauri 接口由 mock.mjs 模拟。
//
// 用法：npm run dev 之后执行 node tests/ui/usage.e2e.mjs
import { createRequire } from "node:module";
import { MOCK } from "./mock.mjs";

const [, , urlArg, pwDirArg] = process.argv;
const url = urlArg ?? "http://localhost:1430/#/usage";
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

console.log("\n[0] 用量页：汇总与各维度");
await page.getByRole("heading", { name: "用量", exact: true }).waitFor();
check("今天 / 近 24 小时 / 累计的数字都在", await waitFor(async () =>
  (await page.locator("[data-testid=usage-today]").getByText("$0.05").count()) === 1));
check("按仓库列出花费", await waitFor(async () => (await page.locator("[data-testid=usage-by-repo]").getByText("octo/demo").count()) === 1));
check("按规则列出花费", await waitFor(async () => (await page.locator("[data-testid=usage-by-rule]").getByText("demo 的 bug 自动修").count()) === 1));
check("按 Agent / 按机器的表都在", (await page.locator("[data-testid=usage-by-agent]").getByText("claude-cli").count()) === 1
  && (await page.locator("[data-testid=usage-by-machine]").getByText("本机").count()) === 1);
check("趋势有柱子", (await page.locator("[data-testid=usage-trend] div.rounded-sm").count()) === 2);

console.log("\n[1] 每日预算：设置 → 进度 → 超限");
check("未设置时给引导", (await page.getByText("设了预算，超了之后自动化规则和守护修复就不再自动开工", { exact: false }).count()) === 1);
await page.getByTestId("usage-budget-input").fill("5");
await page.getByTestId("usage-budget-save").click();
check("保存后出现进度条", await waitFor(async () => (await page.locator("[data-testid=usage-budget-bar]").count()) === 1));
check("显示今天 / 预算", (await page.getByText("今天 $0.05 / 预算 $5.00", { exact: true }).count()) === 1);
// 把预算调到已花之下：状态变成「已超」
await page.getByTestId("usage-budget-input").fill("0.01");
await page.getByTestId("usage-budget-save").click();
check("超了显示「已超」", await waitFor(async () => (await page.locator("[data-testid=usage-budget]").getByText("已超", { exact: true }).count()) === 1));
// 非法值会被拦
await page.getByTestId("usage-budget-input").fill("abc");
await page.getByTestId("usage-budget-save").click();
check("非法值提示原因", await waitFor(async () => page.getByText(/大于 0 的数字/).isVisible()));

const failed = results.filter((r) => !r).length;
console.log(`\nusage.e2e: ${results.length - failed}/${results.length} 项通过${failed ? "" : "，全绿"}${pageErrors.length ? `；页面报错 ${pageErrors.length} 条` : ""}`);
if (pageErrors.length) console.log(pageErrors.slice(0, 5).join("\n"));
await browser.close();
process.exit(failed || pageErrors.length ? 1 : 0);
