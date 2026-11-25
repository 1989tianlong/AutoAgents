// 在无头 WebKit 里验证部署页：连接 Vercel / Cloudflare、登记项目、跟踪每次部署的状态与健康，
// 发起部署、看构建日志、取消、重做、提升与回滚，以及生产出问题时链到修复任务、任务页链回部署。
// Tauri 接口由 mock.mjs 模拟。
//
// 用法：npm run dev 之后执行 npm run test:ui（或单独 node tests/ui/deploy.e2e.mjs）
import { createRequire } from "node:module";
import { MOCK } from "./mock.mjs";

const [, , urlArg, pwDirArg] = process.argv;
const url = urlArg ?? "http://localhost:1430/#/deploy";
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
const calls = (cmd) => page.evaluate((c) => window.__mock.state.calls.filter((x) => x.cmd === c).map((x) => x.args), cmd);
/** 左边清单里的一个项目 */
const item = (name) =>
  page.locator("main aside button").filter({ has: page.locator("span", { hasText: new RegExp(`^${name}$`) }) }).first();
/** 部署记录里的一行 */
const row = (text) => page.locator("main section table tbody tr").filter({ hasText: text }).first();
const rows = () => page.locator("main section table tbody tr");
const dialog = () => page.locator("div.fixed.inset-0");
/** 操作之后留在页面上的结果 */
const notice = () => page.locator('main [role="status"]');
const has = (text) => new RegExp(text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"));
/** 「部署到生产」或「部署预览」 */
const deployBtn = () => dialog().getByRole("button", { name: /^部署到生产$|^部署预览$/ });
const closeDialog = () => waitFor(async () => (await dialog().count()) === 0);
/** 登记后项目在本地清单里的编号 */
const idOf = (name) => page.evaluate((n) => window.__mock.state.deploy.targets.find((t) => t.name === n)?.id, name);
/** 在某个项目里等某个状态出现：页面每 5 秒对一次，点「刷新」能马上对 */
const open = async (name) => {
  await item(name).click();
  await page.getByRole("heading", { name, exact: true }).waitFor();
};

console.log("\n[0] 连接平台");
await page.getByRole("heading", { name: "部署", exact: true }).waitFor();
await page.getByText("选一个项目", { exact: true }).waitFor();
// 卡片先按「没连上」画，状态查回来才换：等它换完再看
await page.getByText("已连接 · octo", { exact: true }).waitFor();
check("已连接的 Vercel 标出账号", await page.getByText("已连接 · octo", { exact: true }).isVisible());
check("没连的 Cloudflare 请你填入令牌", (await page.getByText("未连接", { exact: true }).count()) === 1 && (await page.getByRole("button", { name: "填入令牌" }).count()) === 1);
await page.getByRole("button", { name: "填入令牌" }).click();
await page.getByRole("heading", { name: "连接 Cloudflare" }).waitFor();
await dialog().getByLabel("API 令牌").fill("nope");
await dialog().getByRole("button", { name: "验证并保存" }).click();
check(
  "令牌不对：把平台给的原因留在对话框里",
  (await waitFor(() => dialog().getByRole("alert").isVisible())) && (await dialog().getByRole("alert").innerText()).includes("令牌无效或已过期"),
);
const conn = (await lastCall("deploy_connect"))?.args;
check("deploy_connect 带平台与令牌", conn?.platform === "cloudflare" && conn.token === "nope", JSON.stringify(conn));
await dialog().getByLabel("API 令牌").fill("cf_good");
await dialog().getByRole("button", { name: "验证并保存" }).click();
check("连上后关掉对话框", await closeDialog());
check("Cloudflare 也已连接，并出现账户选择", await waitFor(() => page.getByLabel("Cloudflare 的团队或账户").isVisible()) && (await page.getByText("已连接", { exact: true }).isVisible()));

console.log("\n[1] 换账户");
await page.getByLabel("Cloudflare 的团队或账户").selectOption("acct2");
const scope = (await lastCall("deploy_set_scope"))?.args;
check("deploy_set_scope 带平台与账户", scope?.platform === "cloudflare" && scope.scope === "acct2", JSON.stringify(scope));
check("选择留在了下拉框里", (await page.getByLabel("Cloudflare 的团队或账户").inputValue()) === "acct2");

console.log("\n[2] 从平台上登记项目");
await page.getByRole("button", { name: "＋ 登记" }).click();
await page.getByRole("heading", { name: "登记项目" }).waitFor();
await dialog().getByRole("checkbox", { name: "api" }).waitFor();
check("已经登记过的不能再选", (await dialog().getByRole("checkbox", { name: "web" }).isDisabled()) && (await dialog().getByText("已登记", { exact: true }).count()) === 1);
await dialog().getByRole("checkbox", { name: "api" }).check();
await dialog().getByRole("button", { name: "登记 1 个" }).click();
await closeDialog();
const first = (await calls("deploy_target_add"))[0];
check("deploy_target_add 带项目在平台上的名字", first?.provider === "vercel" && first.name === "api" && first.platformId === "prj_api", JSON.stringify(first));
await page.getByRole("heading", { name: "api", exact: true }).waitFor();
const apiId = await idOf("api");
await item("api").waitFor();
check("登记完直接打开它", page.url().includes(`t=${apiId}`) && (await item("api").count()) === 1);

// 第二轮：Cloudflare 的一个 Pages 项目和一个 Worker。Worker 没有关联仓库，生产地址要自己给
await page.getByRole("button", { name: "＋ 登记" }).click();
await page.getByRole("heading", { name: "登记项目" }).waitFor();
await dialog().getByRole("button", { name: "Cloudflare", exact: true }).click();
await dialog().getByRole("checkbox", { name: "docs" }).check();
await dialog().getByRole("checkbox", { name: "api-worker" }).check();
await dialog().getByLabel("api-worker 的生产地址").fill("https://api.example.com");
await dialog().getByRole("button", { name: "登记 2 个" }).click();
await closeDialog();
const added = (await calls("deploy_target_add")).slice(1);
check(
  "Pages 与 Workers 各登记一个",
  added.length === 2 && added[0]?.name === "docs" && added[1]?.name === "api-worker" && added[1]?.provider === "cloudflare-workers" && added[1]?.url === "https://api.example.com" && added[1]?.hookUrl === null,
  JSON.stringify(added),
);
await page.getByRole("heading", { name: "api-worker", exact: true }).waitFor();
check("Worker 标出来，并记下生产地址", (await page.locator("main section span", { hasText: /^Workers$/ }).first().isVisible()) && (await page.getByRole("button", { name: "api.example.com ↗" }).isVisible()));
const [docsId, workerId] = [await idOf("docs"), await idOf("api-worker")];

console.log("\n[3] 部署记录");
await open("web");
const cur = row("ship it");
await cur.waitFor();
check("概要：线上是哪次提交、健康如何", (await page.getByText(/线上是/).innerText()).match(/abcdef1.*ship it.*正常/));
check("列出这个项目的五次部署", (await rows().count()) === 5);
check("当前生产：成功、当前、健康正常", (await cur.locator("span", { hasText: /^当前$/ }).count()) === 1 && (await cur.locator("span", { hasText: /^正常$/ }).count()) === 1 && /生产/.test(await cur.innerText()));
check("耗时从提交算到就绪", (await cur.innerText()).includes("1 分 30 秒"));
check("左边标出最近一次部署的状态", (await item("web").locator("span.bg-emerald-400").count()) === 1);
const fail = row("npm run build");
check("失败的部署把原因写在提交那一栏", (await fail.innerText()).includes('Command "npm run build" exited with 1'));
const build = row("work in progress");
check("进行中的部署可以取消", (await build.locator("span", { hasText: /^构建中$/ }).first().isVisible()) && (await build.getByRole("button", { name: "取消", exact: true }).count()) === 1);
const preview = row("给部署页补测试");
check("预览部署：能打开、能升为生产", (await preview.getByRole("button", { name: "打开", exact: true }).count()) === 1 && (await preview.getByRole("button", { name: "升为生产" }).count()) === 1 && /预览/.test(await preview.innerText()));
const prev = row("previous release");
check("上一次成功的生产部署可以回去", await prev.getByRole("button", { name: "回滚到这一次" }).isVisible());
await fail.getByRole("button", { name: "修复任务 #3 →" }).click();
await page.getByRole("heading", { name: "升级依赖" }).waitFor();
check("从失败的部署跳到为它建的修复任务", page.url().includes("/tasks?t=3"));
await page.evaluate(() => { location.hash = "#/deploy?t=1"; });
await page.getByRole("heading", { name: "web", exact: true }).waitFor();
await cur.waitFor();

console.log("\n[4] 发起部署：生产要确认，预览不用");
await page.getByRole("button", { name: "部署…" }).click();
await page.getByRole("heading", { name: "部署 web" }).waitFor();
check(
  "默认是生产分支：要先勾选确认",
  (await dialog().getByLabel("分支", { exact: true }).inputValue()) === "main" && (await deployBtn().isDisabled()) && (await dialog().getByText("我确认要部署到生产环境").isVisible()),
);
await dialog().getByLabel("分支", { exact: true }).fill("feature/y");
check(
  "换成别的分支就是预览：不用确认",
  (await deployBtn().innerText()) === "部署预览" && (await dialog().getByText("我确认要部署到生产环境").count()) === 0 && !(await deployBtn().isDisabled()),
);
await dialog().getByLabel("分支", { exact: true }).fill("main");
await dialog().getByLabel("提交（可选，留空用分支上最新的").fill("deadbeeef");
check("改了分支，确认要重新勾", await deployBtn().isDisabled());
await dialog().getByRole("checkbox").check();
await deployBtn().click();
await closeDialog();
const trig = (await lastCall("deploy_trigger"))?.args;
check("deploy_trigger 带分支、提交与确认", trig?.targetId === 1 && trig.branch === "main" && trig.commit === "deadbeeef" && trig.confirmed === true, JSON.stringify(trig));
check(
  "发起后向平台对一次，并留下提示",
  (await lastCall("deploy_refresh"))?.args.targetId === 1 && (await notice().innerText()).includes("部署：已发给平台"),
);
check(
  "新的部署排在最上面、排队中",
  await waitFor(() => rows().first().locator("span", { hasText: /^排队中$/ }).first().isVisible()) && (await rows().first().innerText()).includes("deadbee"),
);
await page.getByRole("button", { name: "部署…" }).click();
await page.getByRole("heading", { name: "部署 web" }).waitFor();
await dialog().getByLabel("分支", { exact: true }).fill("feature/x");
await deployBtn().click();
await closeDialog();
const previewTrig = (await lastCall("deploy_trigger"))?.args;
check("预览部署不带确认", previewTrig?.branch === "feature/x" && previewTrig?.commit === null && previewTrig?.confirmed === false, JSON.stringify(previewTrig));

console.log("\n[5] 构建日志、取消、重做");
await fail.getByRole("button", { name: "日志", exact: true }).click();
check("日志面板取来这次部署的日志", await waitFor(() => page.getByText("--- 构建 dpl_fail ---").isVisible()) && (await page.getByText("Type error: Cannot find name 'useEffect'").isVisible()));
check("deploy_logs 带部署编号", (await lastCall("deploy_logs"))?.args.id === "dpl_fail");
await page.getByRole("button", { name: "收起" }).click();
await waitFor(async () => (await page.getByText("--- 构建 dpl_fail ---").count()) === 0);
await build.locator("td").first().click();
check("点一行也能看它的日志", await waitFor(() => page.getByText("--- 构建 dpl_build ---").isVisible()));
await page.getByRole("button", { name: "收起" }).click();
await waitFor(async () => (await page.getByText("--- 构建 dpl_build ---").count()) === 0);
await build.getByRole("button", { name: "取消", exact: true }).click();
check("取消前说清楚", await dialog().getByText("取消这次还在进行的部署？平台会停掉正在跑的构建。").isVisible());
await dialog().getByRole("button", { name: "取消部署", exact: true }).click();
check("deploy_cancel 带部署编号", (await lastCall("deploy_cancel"))?.args.id === "dpl_build");
check("取消后状态跟着变", await waitFor(() => build.locator("span", { hasText: /^已取消$/ }).first().isVisible()));
await fail.getByRole("button", { name: "重做", exact: true }).click();
check("重做生产部署前说清楚", await dialog().getByText(/1111111\s+这次生产部署原样重做一遍/).isVisible());
await dialog().getByRole("button", { name: "确认重做" }).click();
const retry = (await lastCall("deploy_retry"))?.args;
check("deploy_retry 带确认", retry?.id === "dpl_fail" && retry.confirmed === true, JSON.stringify(retry));
check("多了一条排队中的记录", await waitFor(async () => (await rows().count()) === 8));
await build.getByRole("button", { name: "重做", exact: true }).click();
const retryPreview = (await lastCall("deploy_retry"))?.args;
check("重做预览部署不用确认", retryPreview?.id === "dpl_build" && retryPreview.confirmed === false, JSON.stringify(retryPreview));

console.log("\n[6] 升为生产、回滚");
await preview.getByRole("button", { name: "升为生产" }).click();
check("提升前说明用的是预览的那份产物", await dialog().getByText(/直接提升为生产/).isVisible());
await dialog().getByRole("button", { name: "提升为生产", exact: true }).click();
check("deploy_promote 带部署编号", (await lastCall("deploy_promote"))?.args.id === "dpl_preview");
check("提升后它成了当前生产", await waitFor(() => preview.locator("span", { hasText: /^当前$/ }).first().isVisible()) && (await cur.locator("span", { hasText: /^当前$/ }).count()) === 0);
await page.getByRole("button", { name: "回滚…" }).click();
await page.getByRole("heading", { name: "回滚 web" }).waitFor();
await dialog().getByText("previous release").waitFor();
check(
  "只列成功的生产部署；当前的那次不能选",
  (await dialog().locator("input[type=radio]").count()) === 3 && (await dialog().locator("input[type=radio]:disabled").count()) === 1,
);
check("说明回滚的后果", await dialog().getByText(/回滚后 Vercel 会关闭生产域名的自动分配/).isVisible());
await dialog().getByText("previous release").click();
const back = dialog().getByRole("button", { name: "回滚到这一次" });
check("回滚前要确认", (await back.isDisabled()) && (await dialog().getByText("我确认把生产环境切回去").isVisible()));
await dialog().getByRole("checkbox").check();
await back.click();
await closeDialog();
const backArgs = (await lastCall("deploy_rollback"))?.args;
check("deploy_rollback 带项目与那一次部署", backArgs?.targetId === 1 && backArgs.toId === "dpl_prev" && backArgs.versionId === null, JSON.stringify(backArgs));
check("回滚后当前生产换了", (await waitFor(() => prev.locator("span", { hasText: /^当前$/ }).first().isVisible())) && (await notice().innerText()).includes("回滚：已发给平台"));

console.log("\n[7] Worker：Deploy Hook、按版本回滚");
await page.evaluate((targetId) => {
  const s = window.__mock.state;
  const base = s.deploy.deployments.find((d) => d.id === "pg_ok");
  s.deploy.deployments.unshift(
    { ...base, id: "wk_cur", targetId, provider: "cloudflare-workers", project: "api-worker", commitSha: "9e9e9e9", commitMessage: "worker live" },
    { ...base, id: "wk_v", targetId, provider: "cloudflare-workers", project: "api-worker", current: false, commitSha: "8e8e8e8", commitMessage: "worker v1", createdMs: base.createdMs - 60_000 },
  );
}, workerId);
await open("api-worker");
const hook = "https://api.cloudflare.com/client/v4/workers/builds/deploy_hooks/hook-1";
await page.getByRole("button", { name: "部署…" }).click();
await page.getByRole("heading", { name: "部署 api-worker" }).waitFor();
await dialog().getByRole("checkbox").check();
check("没配 Deploy Hook：不能触发，并说明去哪里配", (await deployBtn().isDisabled()) && (await dialog().getByText(/还没有配 Deploy Hook/).isVisible()));
await dialog().getByRole("button", { name: "取消", exact: true }).click();
await closeDialog();
await page.getByLabel(/Deploy Hook 地址/).fill(hook);
await page.getByRole("button", { name: "保存设置" }).click();
check("Hook 存下来了，地址不再显示", await waitFor(() => page.getByText(/Deploy Hook 地址（已保存；留空不改）/).first().isVisible()));
const savedHook = (await calls("deploy_target_update")).at(-1);
check("deploy_target_update 带 Hook 地址", savedHook?.id === workerId && savedHook.hookUrl === hook, JSON.stringify(savedHook));
await page.getByRole("button", { name: "部署…" }).click();
await page.getByRole("heading", { name: "部署 api-worker" }).waitFor();
await dialog().getByRole("checkbox").check();
await deployBtn().click();
await closeDialog();
const workerTrig = (await lastCall("deploy_trigger"))?.args;
check("Worker 一律按生产部署：带确认、不带分支", workerTrig?.targetId === workerId && workerTrig.branch === null && workerTrig.commit === null && workerTrig.confirmed === true, JSON.stringify(workerTrig));
check("两次构建都列出来了", await waitFor(async () => (await rows().count()) === 3));
await page.getByRole("button", { name: "回滚…" }).click();
await page.getByRole("heading", { name: "回滚 api-worker" }).waitFor();
await dialog().getByText("worker v1").waitFor();
await dialog().getByText("worker v1").click();
await dialog().getByRole("checkbox").check();
await dialog().getByRole("button", { name: "回滚到这一次" }).click();
await closeDialog();
const workerBack = (await lastCall("deploy_rollback"))?.args;
check("Worker 回滚按版本号", workerBack?.targetId === workerId && workerBack.toId === "wk_v" && workerBack.versionId === "ver1", JSON.stringify(workerBack));

console.log("\n[8] 健康复查与守护设置");
await open("web");
await row("ship it").waitFor();
// 线上挂了：复查也不通过
await page.evaluate(() => {
  window.__mock.state.deploy.deployments.find((d) => d.id === "dpl_prev").health = "503 Service Unavailable";
  window.__mock.state.deploy.unhealthy.push("dpl_prev");
});
await page.getByRole("button", { name: "刷新", exact: true }).click();
check("健康异常标在那次部署上", await waitFor(() => prev.locator("span", { hasText: /^503 Service Unavailable$/ }).evaluate((el) => el.className.includes("text-rose-400")).catch(() => false)));
await prev.getByRole("button", { name: "复查", exact: true }).click();
check("复查不过：结果留在页面上", await waitFor(() => notice().innerText().then((t) => t.includes("健康检查失败") && t.includes("503"))));
check("deploy_check_health 带部署编号", (await lastCall("deploy_check_health"))?.args.id === "dpl_prev");
await page.evaluate(() => { window.__mock.state.deploy.unhealthy.length = 0; });
await prev.getByRole("button", { name: "复查", exact: true }).click();
check("恢复后显示正常", await waitFor(async () => (await prev.locator("span", { hasText: /^正常$/ }).count()) === 1));

await page.getByRole("checkbox", { name: "在这里发起生产部署要先确认" }).uncheck();
await page.getByLabel("健康检查路径").fill("/healthz");
await page.getByLabel("期望包含").fill("ok");
await page.getByLabel("生产地址", { exact: true }).fill("https://demo2.example.com");
await page.getByRole("button", { name: "保存设置" }).click();
const saved = (await calls("deploy_target_update")).at(-1);
check(
  "deploy_target_update 带上每一项设置",
  saved?.id === 1 && saved.prodRequiresApproval === false && saved.autoFix === true && saved.healthPath === "/healthz" && saved.healthContains === "ok" && saved.url === "https://demo2.example.com" && saved.hookUrl === undefined,
  JSON.stringify(saved),
);
check(
  "保存后生产地址换了",
  (await waitFor(() => page.getByRole("button", { name: "demo2.example.com ↗" }).isVisible())) && (await notice().innerText()).includes("设置已保存"),
);
await page.getByRole("button", { name: "部署…" }).click();
await page.getByRole("heading", { name: "部署 web" }).waitFor();
check("关掉确认后，生产部署可以直接发", (await dialog().getByText("我确认要部署到生产环境").count()) === 0 && !(await deployBtn().isDisabled()));
await deployBtn().click();
await closeDialog();
const free = (await lastCall("deploy_trigger"))?.args;
check("deploy_trigger 不带确认", free?.targetId === 1 && free.confirmed === false, JSON.stringify(free));

console.log("\n[9] Cloudflare Pages");
await page.evaluate(() => {
  const s = window.__mock.state;
  const base = s.deploy.deployments.find((d) => d.id === "pg_ok");
  s.deploy.deployments.unshift({ ...base, id: "pg_build", state: "building", rawState: "BUILDING", current: false, commitSha: "7c7c7c7", commitMessage: "pages wip", readyMs: null });
});
await open("site");
const pagesBuild = row("pages wip");
check("Pages 没有取消部署的接口：不给出取消", (await waitFor(() => pagesBuild.isVisible())) && (await pagesBuild.getByRole("button", { name: "取消", exact: true }).count()) === 0);
await page.getByRole("button", { name: "部署…" }).click();
await page.getByRole("heading", { name: "部署 site" }).waitFor();
check("说明 Pages 是把最近一次原样重做", await dialog().getByText(/没有指定分支发起部署的接口/).isVisible());
await dialog().getByRole("button", { name: "取消", exact: true }).click();
await closeDialog();

console.log("\n[10] 任务页：标出这个任务的部署");
await page.evaluate(() => { location.hash = "#/tasks?t=2"; });
await page.getByRole("heading", { name: "给部署页补测试" }).waitFor();
/** 任务详情里一个部署小标签 */
const chip = () => page.locator("main section span.inline-flex", { hasText: "web" }).first();
check("这条分支的部署列在任务下面", await waitFor(() => chip().isVisible()) && /成功/.test(await chip().innerText()) && (await chip().innerText()).includes("web"));
await page.evaluate(() => { location.hash = "#/tasks?t=3"; });
await page.getByRole("heading", { name: "升级依赖" }).waitFor();
check("修复任务在推提交之前就挂着触发它的那次部署", await waitFor(() => chip().isVisible()) && /失败/.test(await chip().innerText()));
await chip().getByRole("button", { name: "详情 →" }).click();
await page.getByRole("heading", { name: "web", exact: true }).waitFor();
check("点详情跳到那个项目的部署页", page.url().includes("/deploy?t=1"));

console.log("\n[11] 取消登记、断开");
await open("docs");
await page.getByText(/还没有部署记录/).waitFor();
check("还没部署过的项目", await page.getByText(/还没有部署记录/).isVisible());
await page.getByRole("button", { name: "取消登记", exact: true }).click();
check("说清楚只删本地记录", await dialog().getByText(has("取消登记「docs」：本地的部署记录会删掉，平台上的项目和线上服务都不动。")).isVisible());
await dialog().getByRole("button", { name: "取消登记", exact: true }).click();
check("deploy_target_remove 带项目", (await lastCall("deploy_target_remove"))?.args.id === docsId);
check("清单里少了一个", await waitFor(async () => (await item("docs").count()) === 0));
check("回到未选择的状态", await waitFor(() => page.getByText("选一个项目", { exact: true }).isVisible()));
// 两张卡片里第二张是 Cloudflare
await page.getByRole("button", { name: "断开", exact: true }).nth(1).click();
check("deploy_disconnect 带平台", (await lastCall("deploy_disconnect"))?.args.platform === "cloudflare" && (await waitFor(async () => (await page.getByText("未连接", { exact: true }).count()) === 1)));
await page.getByRole("button", { name: "＋ 登记" }).click();
await page.getByRole("heading", { name: "登记项目" }).waitFor();
check("断开的平台不能从中登记", await dialog().getByRole("button", { name: "Cloudflare（未连接）" }).isDisabled());
await dialog().getByRole("button", { name: "取消", exact: true }).click();
await closeDialog();

console.log("\n[12] 活动页记下部署与任务的每一步；工作流页指向已上线的发布守护");
await page.evaluate(() => {
  window.__mock.state.events = [
    { id: 3, ts: "2026-11-24 03:00:05", kind: "deploy.repair", level: "warning", source: "web", message: "生产部署失败，已建修复任务 #3", link: "#/tasks?t=3" },
    { id: 2, ts: "2026-11-24 03:00:00", kind: "deploy.failed", level: "error", source: "web", message: "生产部署失败：abcdef1 ship it", link: "#/deploy?t=1" },
    { id: 1, ts: "2026-11-23 22:00:00", kind: "task.merged", level: "info", source: "给首页加缓存", message: "PR #104 已合并", link: "#/tasks?t=4" },
  ];
});
await page.evaluate(() => { location.hash = "#/activity"; });
await page.getByRole("heading", { name: "活动", exact: true }).waitFor();
await page.getByText("建修复任务", { exact: true }).first().waitFor();
/** 事件左边那个归类标签 */
const kindTag = (label) => page.locator("main span", { hasText: new RegExp(`^${label}$`) }).first();
check("部署与任务的事件都有中文文案", (await kindTag("部署失败").isVisible()) && (await kindTag("PR 已合并").isVisible()) && (await page.getByText(/^deploy\./).count()) === 0);
check("出错标红、要留意标黄", (await kindTag("部署失败").evaluate((el) => el.className.includes("rose"))) && (await kindTag("建修复任务").evaluate((el) => el.className.includes("amber"))));
await page.getByRole("button", { name: /^部署\s*2$/ }).click();
await waitFor(async () => (await page.locator("div.gap-3.px-4").count()) === 2);
check("按部署归类筛选", (await page.getByText("PR #104 已合并").count()) === 0);
await page.locator("div.gap-3.px-4", { hasText: "已建修复任务" }).getByRole("button", { name: "查看 →" }).click();
await page.getByRole("heading", { name: "升级依赖" }).waitFor();
check("从活动跳到修复任务", page.url().includes("/tasks?t=3"));
await page.evaluate(() => { location.hash = "#/workflows"; });
await page.getByRole("heading", { name: "工作流", exact: true }).waitFor();
check("发布守护标成已上线，其余三个模板都能建流", await waitFor(async () =>
  (await page.getByRole("link", { name: "已上线 · 去部署页 →" }).count()) === 1
  && (await page.getByRole("button", { name: "用这个模板" }).count()) === 3));
await page.getByRole("link", { name: "已上线 · 去部署页 →" }).click();
await page.getByRole("heading", { name: "部署", exact: true }).waitFor();
check("点了就到部署页", page.url().includes("/deploy"));

const failed = results.filter((r) => !r).length;
console.log(`\n${results.length - failed}/${results.length} 项通过${pageErrors.length ? `；页面错误 ${pageErrors.length} 条：` : ""}`);
for (const e of pageErrors.slice(0, 8)) console.log("  [page] " + e.slice(0, 300));
await browser.close();
process.exit(failed || pageErrors.length ? 1 : 0);
