// 在无头 WebKit 里验证多机相关的界面：机器页（中继、接入、状态、管理动作）、
// 跨机器的 Agents 与会话、活动记录、仪表盘、设置。
// Tauri 接口全部由 mock.mjs 模拟。
//
// 用法：npm run dev 之后执行 npm run test:ui（或单独 node tests/ui/machines.e2e.mjs）
import { createRequire } from "node:module";
import { MOCK } from "./mock.mjs";

const [, , urlArg, pwDirArg] = process.argv;
const url = urlArg ?? "http://localhost:1430/#/machines";
const require = createRequire(import.meta.url);
const pwDir = pwDirArg ?? process.env.PLAYWRIGHT_CORE ?? "playwright-core";
const { webkit } = require(pwDir);

const results = [];
const check = (name, cond, extra = "") => {
  results.push(!!cond);
  console.log(`  ${cond ? "✓" : "✗"} ${name}${cond || !extra ? "" : `  ← ${extra}`}`);
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const waitFor = async (fn, ms = 7000) => {
  const t0 = Date.now();
  while (Date.now() - t0 < ms) {
    if (await fn()) return true;
    await sleep(80);
  }
  return false;
};

const browser = await webkit.launch();
const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
const pageErrors = [];
page.on("pageerror", (e) => pageErrors.push(String(e)));
page.on("console", (m) => { if (m.type() === "error") pageErrors.push(m.text()); });
page.on("dialog", (dialog) => dialog.accept());
await page.addInitScript(MOCK);
await page.goto(url);

const RELAY = "autoagents://relay?addr=wss://relay.example.com:7000&token=s3cret-token&fp=sha256:0011223344556677889900112233445566778899001122334455667788990011";

/** 跳到某个页面（改 hash），等到它的标题出现 */
const goto = async (hash, title) => {
  await page.evaluate((h) => { location.hash = h; }, hash);
  await page.getByRole("heading", { name: title, exact: true }).waitFor();
};
/** 机器卡片：按机器名找 */
const card = (name) => page.locator("div.flex.flex-col.p-5", { hasText: name }).first();
const lastCall = (cmd) => page.evaluate((c) => window.__mock.lastCall(c), cmd);

console.log("\n[0] 机器页：本机卡片、资源占用、没有中继时的提示");
await goto("#/machines", "机器");
await page.getByText("桌面应用内置 v0.2.0").waitFor();
check("本机卡片显示机器侧服务", await page.getByText("桌面应用内置 v0.2.0").isVisible());
check("本机标为「本机」、不是离线", (await card("本机").getByText("本机", { exact: true }).count()) >= 1 && (await card("本机").getByText("离线", { exact: true }).count()) === 0);
check("本机卡片不能被移除", (await card("本机").getByRole("button", { name: "移除" }).count()) === 0);
check("显示资源占用", await waitFor(() => card("本机").getByText(/^24%/).isVisible()));
check("显示内存用量", await waitFor(() => card("本机").getByText("8.0 GB / 16.0 GB").isVisible()));
check("没有中继时提示去设置", await page.getByText("还没有设置中继").isVisible());
check(
  "统计：1 台机器、4 个 Agent",
  await waitFor(async () => (await page.getByText("在线 1").count()) === 1 && (await page.getByText("所有机器合计").count()) === 1),
);

console.log("\n[1] 设置中继：粘贴连接串");
await page.getByRole("button", { name: "中继设置" }).click();
await page.getByRole("heading", { name: "中继设置" }).waitFor();
check("未设置时展开搭建说明", await page.getByText("还没有中继？一台有公网地址的服务器就够了").isVisible());
await page.getByPlaceholder(/autoagents:\/\/relay/).fill(RELAY);
await page.getByRole("button", { name: "保存" }).click();
await waitFor(async () => !(await page.getByRole("heading", { name: "中继设置" }).isVisible()));
const relaySet = await lastCall("relay_set");
check("relay_set 收到整段连接串", relaySet?.args.text === RELAY, JSON.stringify(relaySet?.args));
check("页面显示中继地址", await waitFor(() => page.getByText("wss://relay.example.com:7000").first().isVisible()));
check("显示已连接", await waitFor(() => page.getByText("已连接", { exact: true }).first().isVisible()));
check("令牌不会出现在界面上", !(await page.content()).includes("s3cret-token"));
check("没有中继的提示消失", !(await page.getByText("还没有设置中继").isVisible()));

console.log("\n[2] 接入新机器：生成安装命令，等它连上后自动配对");
await page.getByRole("button", { name: "＋ 接入新机器" }).click();
await page.getByRole("heading", { name: "接入新机器" }).waitFor();
await page.getByPlaceholder(/work-pc/).fill("home-mac");
await page.getByPlaceholder(/opt\/apps/).fill("~/code\n/srv/app");
await page.getByRole("button", { name: "生成安装命令" }).click();
await page.locator("code", { hasText: "install --name 'home-mac'" }).waitFor();
const install = await lastCall("node_install_command");
check("名字与允许访问的目录传给 Rust", install?.args.name === "home-mac" && JSON.stringify(install?.args.allow) === JSON.stringify(["~/code", "/srv/app"]), JSON.stringify(install?.args));
const shown = await page.locator("code", { hasText: "install --name" }).innerText();
check("命令里带配对码、中继地址与允许的目录", shown.includes("--pair ABCD-EFGH-JKLM-NPQR") && shown.includes("--relay 'wss://relay.example.com:7000'") && shown.includes("--allow '~/code' --allow '/srv/app'"), shown);
check("显示等待接入与倒计时", await page.getByText(/等待接入 · 配对码 \d\d:\d\d 内有效/).isVisible());
await page.getByRole("button", { name: "复制" }).click();
check("一键复制后给出反馈", await waitFor(() => page.getByRole("button", { name: "已复制" }).isVisible()));
check("页面上列出未用的邀请", await waitFor(() => page.getByText("等待接入：").isVisible()));
// 那台机器执行了安装命令、连上中继
await page.evaluate(() => window.__mock.pairPending("home-mac"));
check("接入成功后提示并自动关闭", await waitFor(() => page.getByText(/已接入「home-mac」/).isVisible()));
check("对话框随后自动关闭", await waitFor(async () => (await page.locator("div.fixed.inset-0").count()) === 0, 5000));
check("新机器出现在清单里", await waitFor(() => card("home-mac").isVisible()));
check("新机器在线、显示延迟", await waitFor(() => card("home-mac").getByText("42 毫秒").isVisible()));
check("统计：2 台都在线", await waitFor(() => page.getByText("在线 2").isVisible()));
check("邀请用掉后不再列出", await waitFor(async () => (await page.getByText("等待接入：").count()) === 0));

console.log("\n[3] 离线的机器：说明原因、只能重连");
await page.evaluate(() => window.__mock.addMachine("srv", "server-1", { online: false, error: "节点 60 秒没有任何消息，判定为断线", agents: [] }));
await waitFor(() => card("server-1").isVisible());
check("离线机器显示原因", await waitFor(() => card("server-1").getByText(/判定为断线/).isVisible()));
check("显示最后在线时间", await card("server-1").getByText("1 小时前").isVisible());
check("离线时不能授权其他设备", await card("server-1").getByRole("button", { name: "授权其他设备" }).isDisabled());
await card("server-1").getByRole("button", { name: "立即重连" }).click();
check("立即重连带机器 id", (await lastCall("node_connect_now"))?.args.nodeId === "srv");
check("统计：3 台、在线 2", await waitFor(() => page.getByText("在线 2").isVisible()));

console.log("\n[4] 重命名");
await card("home-mac").getByRole("button", { name: "重命名" }).click();
await page.getByRole("heading", { name: "重命名" }).waitFor();
await page.locator("input[value='home-mac']").fill("家里的 Mac");
await page.getByRole("button", { name: "保存" }).click();
check("node_rename 收到新名字", (await lastCall("node_rename"))?.args.name === "家里的 Mac");
check("卡片显示新名字", await waitFor(() => card("家里的 Mac").isVisible()));

console.log("\n[5] 授权其他设备：给出配对码");
await card("家里的 Mac").getByRole("button", { name: "授权其他设备" }).click();
await page.getByRole("heading", { name: /授权其他设备 · 家里的 Mac/ }).waitFor();
check("node_authorize 带机器 id", await waitFor(async () => (await lastCall("node_authorize"))?.args.nodeId === "node-home-mac"));
check("显示配对码", await page.locator("code", { hasText: "AUTH-CODE-1234-5678" }).isVisible());
await page.getByRole("button", { name: "关闭" }).click();
await waitFor(async () => !(await page.getByRole("heading", { name: /授权其他设备/ }).isVisible()));

console.log("\n[6] 移除机器");
const before = await page.evaluate(() => window.__mock.state.machines.length);
await card("server-1").getByRole("button", { name: "移除" }).click();
check("node_remove 带机器 id", (await lastCall("node_remove"))?.args.nodeId === "srv");
check("确认之后机器被移除", await waitFor(async () => (await page.evaluate(() => window.__mock.state.machines.length)) === before - 1));
await waitFor(async () => (await card("server-1").count()) === 0);

console.log("\n[7] 中继上有一台没配对的机器：输入配对码配对");
await page.evaluate(() => {
  const relay = window.__mock.state.relay;
  relay.online.push({ nodeId: "lab-0", name: "lab-pc", os: "linux", sinceMs: Date.now() - 5000, paired: false });
  window.__mock.emitNode({ id: "lab-0", name: "lab-pc", online: true, message: null });
});
const relayRow = (name) => page.locator("div.flex.items-center.gap-3", { hasText: name });
// 离线的机器不在中继的在线名单里
check("离线的机器不在中继名单里", (await page.getByText("server-1", { exact: true }).count()) === 0);
await relayRow("lab-pc").getByRole("button", { name: "配对" }).click();
await page.getByRole("heading", { name: "与「lab-pc」配对" }).waitFor();
await page.getByPlaceholder(/XXXX-XXXX/).fill("WRONG-CODE-WRONG-CODE");
const dialog = () => page.locator("div.fixed.inset-0");
await dialog().getByRole("button", { name: "配对", exact: true }).click();
check("配对码不对时显示原因", await waitFor(() => page.getByText(/握手失败/).isVisible()));
await page.getByPlaceholder(/XXXX-XXXX/).fill("WXYZ-WXYZ-WXYZ-WXYZ");
await dialog().getByRole("button", { name: "配对", exact: true }).click();
check("node_pair 带机器 id 与配对码", await waitFor(async () => {
  const call = await lastCall("node_pair");
  return call?.args.nodeId === "lab-0" && call?.args.code === "WXYZ-WXYZ-WXYZ-WXYZ";
}), JSON.stringify(await lastCall("node_pair")));
check("配对后加入清单", await waitFor(() => card("lab-pc").isVisible()));

console.log("\n[8] Agents 页：按机器分组，可以直接在那台机器上开会话");
await goto("#/agents", "Agents");
await page.getByRole("heading", { name: "家里的 Mac", exact: true }).waitFor();
check("按机器分组", await page.getByRole("heading", { name: "本机", exact: true }).isVisible());
check("列出所有机器上的 Agent", (await page.locator("tbody tr").count()) === 6, `行数=${await page.locator("tbody tr").count()}`);
// 在「家里的 Mac」那一组里找，不依赖各组的先后
const homeGroup = page
  .locator("section")
  .filter({ has: page.getByRole("heading", { name: "家里的 Mac", exact: true }) });
await homeGroup.locator("tbody tr", { hasText: "Claude CLI" }).getByRole("button", { name: /会话/ }).click();
await page.getByRole("heading", { name: "与 Agent 对话" }).waitFor();
check(
  "会话页选中的是那台机器",
  await waitFor(() =>
    page
      .getByRole("button", { name: "家里的 Mac", exact: true })
      .evaluate((el) => el.className.includes("bg-sky-950")),
  ),
);
check("只列出那台机器上的 Agent", (await page.locator("button", { hasText: /^(Claude CLI|Codex CLI|Grok CLI|Pi Agent)$/ }).count()) === 1);

console.log("\n[9] 活动页：事件带文案与归类，能跳回会话");
await page.evaluate(() => {
  window.__mock.state.sessions.unshift({ id: 77, adapter: "claude-cli", title: "fix the bug", model: null, workdir: null, nodeId: "local", createdAt: "2026-11-24 01:00:00" });
  window.__mock.state.messages.set(77, [
    { id: 900, sessionId: 77, role: "user", content: "fix the bug", costUsd: null, inputTokens: null, outputTokens: null, durationMs: null, createdAt: "" },
    { id: 901, sessionId: 77, role: "assistant", content: "已修复", costUsd: 0.02, inputTokens: 10, outputTokens: 5, durationMs: 1500, createdAt: "" },
  ]);
  window.__mock.state.events = [
    { id: 5, ts: "2026-11-24 01:00:05", kind: "session.finished", level: "info", source: "本机 · claude-cli", message: "完成一轮：用时 1.5 秒，≈ $0.0200", link: "#/sessions?s=77" },
    { id: 4, ts: "2026-11-24 01:00:00", kind: "session.started", level: "info", source: "本机 · claude-cli", message: "开始一轮：fix the bug", link: "#/sessions?s=77" },
    { id: 3, ts: "2026-11-23 23:00:00", kind: "node.offline", level: "warning", source: "server-1", message: "连接断开：节点 60 秒没有任何消息，判定为断线", link: "#/machines" },
    { id: 2, ts: "2026-11-23 22:00:00", kind: "node.online", level: "info", source: "home-mac", message: "机器已连接，agentd v0.2.0", link: "#/machines" },
  ];
});
await goto("#/activity", "活动");
await page.getByText("完成一轮", { exact: true }).waitFor();
check("事件按时间倒序", (await page.locator("div.gap-3.px-4 > span", { hasText: /2026/ }).first().innerText()) > (await page.locator("div.gap-3.px-4 > span", { hasText: /2026/ }).nth(1).innerText()));
check("机器上线显示为绿色", await page.locator("span", { hasText: "机器上线" }).first().evaluate((el) => el.className.includes("emerald")));
check("离线警告显示为黄色", await page.locator("span", { hasText: "机器离线" }).first().evaluate((el) => el.className.includes("amber")));
check("显示各归类的条数", await page.getByRole("button", { name: /^机器\s*2$/ }).isVisible() && await page.getByRole("button", { name: /^会话\s*2$/ }).isVisible());
await page.getByRole("button", { name: /^机器\s*2$/ }).click();
await waitFor(async () => (await page.locator("div.gap-3.px-4").count()) === 2);
check("按归类筛选", (await page.getByText("开始一轮：fix the bug").count()) === 0 && (await page.getByText(/判定为断线/).count()) === 1);
await page.getByRole("button", { name: /^全部/ }).click();
await page.getByText("开始一轮：fix the bug").waitFor();
await page.locator("div.gap-3.px-4", { hasText: "开始一轮：fix the bug" }).getByRole("button", { name: "查看 →" }).click();
await page.getByText("fix the bug").first().waitFor();
check("点「查看」跳到对应会话", page.url().includes("#/sessions") && await waitFor(async () => (await page.locator(".bg-slate-800.rounded-2xl", { hasText: "已修复" }).count()) === 1));

console.log("\n[10] 仪表盘：真实数字");
await goto("#/", "仪表盘");
await page.getByText(/Agent 共 \d+ 个/).waitFor();
const stats = (await page.locator("div.text-2xl").allInnerTexts()).map((t) => t.replace(/\s+/g, " ").trim());
check("在线机器 3 / 3", stats.includes("3 / 3"), JSON.stringify(stats));
check("近 24 小时花费", stats.includes("$0.050"), JSON.stringify(stats));
check("列出机器", (await page.getByText("lab-pc", { exact: true }).count()) >= 1);
check("最近活动带文案", await waitFor(() => page.getByText("完成一轮", { exact: true }).first().isVisible()));
check("用量图", (await page.locator("div.h-28 > div").count()) === 2);

console.log("\n[11] 设置页：中继状态、安装脚本地址、清空记录");
await goto("#/settings", "设置");
check("显示中继地址与状态", await page.getByText(/wss:\/\/relay\.example\.com:7000 · 已连接/).isVisible());
const scriptInput = page.getByPlaceholder(/install-agentd\.sh/);
check("安装脚本地址是默认值", (await scriptInput.inputValue()).endsWith("scripts/install-agentd.sh"));
await scriptInput.fill("https://mirror.internal/install-agentd.sh");
await page.getByRole("button", { name: "保存" }).click();
check("install_script_set 收到新地址", await waitFor(async () => (await lastCall("install_script_set"))?.args.url === "https://mirror.internal/install-agentd.sh"));
check("保存后给出反馈", await waitFor(() => page.getByText("已保存", { exact: true }).isVisible()));
check("显示活动记录条数", await page.getByText("共 4 条").isVisible());
await page.getByRole("button", { name: "清空" }).click();
check("清空后条数归零", await waitFor(() => page.getByText("还没有记录").isVisible()));

const failed = results.filter((r) => !r).length;
console.log(`\n${results.length - failed}/${results.length} 项通过${pageErrors.length ? `；页面错误 ${pageErrors.length} 条：` : ""}`);
for (const e of pageErrors.slice(0, 8)) console.log("  [page] " + e.slice(0, 300));
await browser.close();
process.exit(failed || pageErrors.length ? 1 : 0);
