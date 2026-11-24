// 在无头 WebKit 里验证会话页的界面逻辑。
// Tauri 接口全部模拟，本脚本扮演 Rust 端：决定何时推送事件、何时让事件"丢失"。
//
// 用法：npm run dev 之后执行 npm run test:ui
//   node tests/ui/sessions.e2e.mjs [页面地址] [playwright-core 所在目录]
// playwright-core 依次从命令行参数、环境变量 PLAYWRIGHT_CORE、项目依赖里查找；
// 首次需要下载 WebKit：npx playwright-core install webkit
import { createRequire } from "node:module";
import { MOCK } from "./mock.mjs";

const [, , urlArg, pwDirArg] = process.argv;
const url = urlArg ?? "http://localhost:1430/#/sessions";
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
const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
const pageErrors = [];
page.on("pageerror", (e) => pageErrors.push(String(e)));
page.on("console", (m) => { if (m.type() === "error") pageErrors.push(m.text()); });
await page.addInitScript(MOCK);
await page.goto(url);

// ---------- 界面里的定位 ----------
const userBubbles = (text) => page.locator(".bg-sky-600.rounded-2xl", { hasText: text });
const assistantBubbles = () => page.locator(".bg-slate-800.rounded-2xl");
// 输出中的光标 "▍" 不算正文
const answerText = async () => (await assistantBubbles().last().innerText()).replace(/▍/g, "").trim();
const systemBubbles = (text) => page.locator(".border-amber-800\\/60", { hasText: text });
const stopButton = () => page.getByRole("button", { name: "停止" });
const modelInput = () => page.getByPlaceholder(/^默认（/);
const messageBox = () => page.getByPlaceholder(/输入消息/);
const sendButton = () => page.getByRole("button", { name: "发送", exact: true });
const statusLine = (text) => page.locator("span.truncate", { hasText: text });
const runningLabels = () => page.locator("button", { hasText: "运行中" });

const emit = (payload) => page.evaluate((p) => window.__mock.emit(p), payload);
const liveSet = (sessionId, text, activity = null) =>
  page.evaluate(([s, t, a]) => window.__mock.liveSet(s, t, a), [sessionId, text, activity]);
const finishTurn = (sessionId, role, content) =>
  page.evaluate(([s, r, c]) => window.__mock.finishTurn(s, r, c), [sessionId, role, content]);
const lastCall = (cmd) => page.evaluate((c) => window.__mock.lastCall(c), cmd);
const callCount = () => page.evaluate(() => window.__mock.callCount());

/** 输入并发送一条消息，等 Rust 端收到 */
async function send(text) {
  const before = await callCount();
  await messageBox().fill(text);
  await sendButton().click();
  await waitFor(async () => (await callCount()) > before);
}

console.log("\n[0] 页面加载与事件订阅");
await page.getByRole("button", { name: "Grok CLI" }).waitFor();
await sleep(300);
const listeners = await page.evaluate(() => window.__mock.listenerCount());
check("页面已订阅 chat-event 且只有一个监听器", listeners === 1, `监听器数=${listeners}`);
check("四个 CLI Agent 都可选", (await page.locator("button", { hasText: /^(Claude CLI|Codex CLI|Grok CLI|Pi Agent)$/ }).count()) === 4);
check("默认工作目录是主目录", (await page.getByPlaceholder("/Users/tester").inputValue()) === "/Users/tester");

console.log("\n[1] 模型选择：按 Agent 读取清单、搜索、选中");
await page.getByRole("button", { name: "Grok CLI" }).click();
await waitFor(async () => (await modelInput().getAttribute("placeholder")) === "默认（magpie/grok/grok-4.7）");
check("Grok 默认模型显示在占位符里", (await modelInput().getAttribute("placeholder")) === "默认（magpie/grok/grok-4.7）");
check("标注模型清单来自哪台机器的哪个配置", await page.getByText("模型清单来自 本机 上的 ~/.grok/config.toml").isVisible());
await modelInput().click();
await page.getByRole("button", { name: /Grok 4\.7 · Grok/ }).waitFor();
const optionCount = await page.locator("div.absolute button").count();
check("展开后列出清单里的全部模型", optionCount === 4, `选项数=${optionCount}（默认 + 3 个模型）`);
await modelInput().fill("glm-5.3-flash");
await waitFor(async () => (await page.locator("div.absolute button").count()) === 2);
check("输入关键字后只剩匹配项", (await page.locator("div.absolute button").allTextContents()).join("|").includes("GLM-5.3-Flash"));
await page.getByRole("button", { name: /GLM-5.3-Flash · routing group/ }).click();
check("点击选项后选中对应模型", (await modelInput().inputValue()) === "GLM-5.3-Flash · routing group");

// 键盘回车 / 辅助功能的"按下"只触发 click（没有 mousedown），也要能选中
await modelInput().click();
await modelInput().fill("zai");
await page.getByRole("button", { name: /GLM-5\.3 · Z\.AI/ }).dispatchEvent("click");
check("仅触发 click 也能选中选项", (await modelInput().inputValue()) === "GLM-5.3 · Z.AI", await modelInput().inputValue());
await modelInput().click();
await modelInput().fill("my-gateway/custom");
await modelInput().press("Enter");
check("没有匹配项时回车确认自定义 ID", (await modelInput().inputValue()) === "my-gateway/custom", await modelInput().inputValue());
await modelInput().click();
await modelInput().fill("glm-5.3-flash");
await modelInput().press("Enter");
check("有匹配项时回车选中第一个匹配项", (await modelInput().inputValue()) === "GLM-5.3-Flash · routing group", await modelInput().inputValue());

console.log("\n[2] 推送正常：流式输出、过程提示、收尾不重复");
await page.getByPlaceholder("/Users/tester").fill("/tmp/project");
await send("hello");
const start = await lastCall("chat_start");
check(
  "chat_start 收到 Agent / 模型 / 工作目录 / 消息",
  start.args.adapter === "grok-cli" && start.args.model === "magpie/auto/glm-5.3-flash" && start.args.workdir === "/tmp/project" && start.args.prompt === "hello",
  JSON.stringify(start.args),
);
await waitFor(async () => (await userBubbles("hello").count()) === 1 && (await stopButton().isVisible()));
await sleep(500);
check("运行中显示停止按钮", await stopButton().isVisible());
check("自己的消息只显示一次", (await userBubbles("hello").count()) === 1, `数量=${await userBubbles("hello").count()}`);
check("等待期间显示状态行", await statusLine("等待响应…").isVisible());

const sid = await page.evaluate(() => window.__mock.state.sessions[0].id);
await liveSet(sid, "", "调用工具 read_file");
check("推送的事件确实到达界面", (await emit({ type: "notice", sessionId: sid, text: "调用工具 read_file" })) === 1);
check("过程提示（工具调用）实时显示", await waitFor(() => statusLine("调用工具 read_file").isVisible()));
await emit({ type: "delta", sessionId: sid, text: "1\n" });
await emit({ type: "delta", sessionId: sid, text: "2" });
check("流式增量实时拼接", await waitFor(async () => (await answerText()) === "1\n2"), JSON.stringify(await page.locator(".bg-slate-800.rounded-2xl").allInnerTexts()));
check("输出中显示光标", (await page.locator(".animate-pulse").count()) === 1);

await finishTurn(sid, "assistant", "1\n2");
await emit({ type: "replace", sessionId: sid, text: "1\n2" });
await emit({ type: "done", sessionId: sid, costUsd: 0.03, inputTokens: 100, outputTokens: 5 });
check("结束后停止按钮消失", await waitFor(async () => !(await stopButton().isVisible())));
await sleep(600);
check("回答只显示一次、内容完整", (await assistantBubbles().count()) === 1 && (await answerText()).startsWith("1\n2"), JSON.stringify(await page.locator(".bg-slate-800.rounded-2xl").allInnerTexts()));
check("显示费用", (await assistantBubbles().innerText()).includes("$0.0300"));
check("结束后不再显示光标", (await page.locator(".animate-pulse").count()) === 0);
check("会话列表不再标记运行中", (await runningLabels().count()) === 0);
check("会话头部显示所选模型", await page.getByText("magpie/auto/glm-5.3-flash", { exact: true }).last().isVisible());

console.log("\n[3] 续聊：第二轮带上会话 id");
await send("second question");
const sendCall = await lastCall("chat_send");
check("chat_send 带上会话 id 与消息", sendCall.args.sessionId === sid && sendCall.args.prompt === "second question", JSON.stringify(sendCall.args));
await waitFor(async () => (await stopButton().isVisible()));
await sleep(500);
check("第二轮自己的消息只显示一次", (await userBubbles("second question").count()) === 1, `数量=${await userBubbles("second question").count()}`);
await finishTurn(sid, "assistant", "answer two");
await emit({ type: "done", sessionId: sid });
check("两轮回答都在", await waitFor(async () => (await assistantBubbles().count()) === 2));

console.log("\n[4] 事件全部丢失（模拟窗口不可见）：靠定时对账显示进度并收尾");
await page.getByRole("button", { name: "＋ 新会话" }).click();
await page.getByRole("button", { name: "Pi Agent" }).click();
await waitFor(async () => (await modelInput().getAttribute("placeholder")) === "默认（magpie/grok/grok-4.7）");
await modelInput().fill("my-gateway/custom-model");
await send("hi pi");
const piCall = await lastCall("chat_start");
check("清单外的模型按自定义 ID 传给 Rust", piCall.args.model === "my-gateway/custom-model" && piCall.args.adapter === "pi", JSON.stringify(piCall.args));
check("自定义模型被记住", await waitFor(() => page.evaluate(() => window.__mock.state.remembered.includes("pi|my-gateway/custom-model"))));
await waitFor(async () => (await stopButton().isVisible()) && (await userBubbles("hi pi").count()) === 1);
await sleep(500);
check("没有事件时显示等待状态", await statusLine("等待响应…").isVisible());
const piId = await page.evaluate(() => window.__mock.state.sessions[0].id);
const t0 = Date.now();
await liveSet(piId, "", "调用工具 bash");
check("对账后显示 Rust 端的过程提示", await waitFor(() => statusLine("调用工具 bash").isVisible(), 5000));
await liveSet(piId, "部分输出", null);
check("对账后显示 Rust 端已有的输出", await waitFor(async () => (await answerText()) === "部分输出", 5000), JSON.stringify(await page.locator(".bg-slate-800.rounded-2xl").allInnerTexts()));
await finishTurn(piId, "assistant", "完整回答");
check("Rust 端结束后界面自动收尾", await waitFor(async () => !(await stopButton().isVisible()), 5000), `用时 ${Date.now() - t0}ms`);
check("收尾后显示落库的完整回答", await waitFor(async () => (await answerText()).startsWith("完整回答")), JSON.stringify(await answerText()));
check("收尾后会话不再标记运行中", (await runningLabels().count()) === 0);

console.log("\n[5] 出错：显示 Rust 端写入的错误，不卡在运行中");
await send("break please");
await waitFor(async () => (await stopButton().isVisible()));
await finishTurn(piId, "system", "已停止");
await emit({ type: "error", sessionId: piId, message: "已停止" });
check("错误以系统消息显示", await waitFor(() => systemBubbles("已停止").isVisible()));
check("出错后不卡在运行中", await waitFor(async () => !(await stopButton().isVisible())));

console.log("\n[6] 停止按钮");
await send("long task");
await waitFor(async () => (await stopButton().isVisible()));
await stopButton().click();
const stopCall = await lastCall("chat_stop");
check("点击停止会调用 chat_stop", stopCall?.args.sessionId === piId, JSON.stringify(stopCall?.args));
await finishTurn(piId, "system", "已停止");
await emit({ type: "error", sessionId: piId, message: "已停止" });
await waitFor(async () => !(await stopButton().isVisible()));

console.log("\n[7] 离开再回到会话页时会话仍在运行：以 Rust 端状态为准");
await page.evaluate((s) => {
  window.__mock.state.live.push({ sessionId: s, text: "后台仍在输出", activity: "调用工具 grep" });
}, piId);
await page.getByRole("link", { name: "仪表盘" }).click();
await page.getByRole("heading", { name: "仪表盘" }).waitFor();
await page.getByRole("link", { name: "会话", exact: true }).click();
check("会话列表显示运行中", await waitFor(async () => (await runningLabels().count()) === 1));
await runningLabels().first().click();
check("打开后显示后台产生的输出", await waitFor(async () => (await answerText()) === "后台仍在输出"));
check("并显示当前过程提示", await statusLine("调用工具 grep").isVisible());

console.log("\n[8] 没有任何会话在运行时，重新进入不会误标运行中");
await page.evaluate(() => { window.__mock.state.live = []; });
await page.getByRole("link", { name: "仪表盘" }).click();
await page.getByRole("heading", { name: "仪表盘" }).waitFor();
await page.getByRole("link", { name: "会话", exact: true }).click();
await page.getByRole("button", { name: "Grok CLI" }).waitFor();
await sleep(2600);
check("没有残留的运行中标记", (await runningLabels().count()) === 0);

console.log("\n[9] 选择机器：Agent、模型清单、工作目录都跟着换到那台机器上");
await page.evaluate(() => window.__mock.addMachine("home-mac", "家里的 Mac", { home: "/Users/home" }));
// 等机器清单刷新后，新会话面板里出现那台机器
await page.getByRole("button", { name: "＋ 新会话" }).click();
const remoteChip = () => page.getByRole("button", { name: "家里的 Mac", exact: true });
const localChip = () => page.getByRole("button", { name: "本机", exact: true });
await remoteChip().waitFor();
await remoteChip().click();
check("选中的机器高亮", await page.locator("button.bg-sky-950\\/50", { hasText: "家里的 Mac" }).first().isVisible());
check("Agent 清单换成那台机器上的", (await page.locator("button", { hasText: /^(Claude CLI|Codex CLI|Grok CLI|Pi Agent)$/ }).count()) === 1);
check("工作目录默认是那台机器的主目录", (await page.getByPlaceholder("/Users/home").inputValue()) === "/Users/home");
await modelInput().click();
await waitFor(async () => (await lastCall("agent_models"))?.args.nodeId === "home-mac");
check("模型清单按机器读取", (await lastCall("agent_models")).args.nodeId === "home-mac", JSON.stringify((await lastCall("agent_models")).args));

console.log("\n[10] 浏览目录：列出那台机器上的目录，选中后填进工作目录");
await page.getByRole("button", { name: "浏览…" }).click();
await page.getByRole("heading", { name: /选择工作目录 · 家里的 Mac/ }).waitFor();
await page.getByRole("button", { name: /code/ }).first().click();
await waitFor(async () => (await lastCall("node_ls"))?.args.path === "/Users/home/code");
check("进入子目录时按那台机器查询", (await lastCall("node_ls")).args.path === "/Users/home/code" && (await lastCall("node_ls")).args.nodeId === "home-mac", JSON.stringify((await lastCall("node_ls")).args));
await page.getByRole("button", { name: "使用这个目录" }).click();
check("选中的目录填进工作目录", (await page.getByPlaceholder("/Users/home").inputValue()) === "/Users/home/code");

await send("remote hello");
const remoteStart = await lastCall("chat_start");
check("chat_start 带上机器 id", remoteStart.args.nodeId === "home-mac" && remoteStart.args.workdir === "/Users/home/code", JSON.stringify(remoteStart.args));
await waitFor(async () => (await stopButton().isVisible()));
check("会话头部标出所在机器", await page.locator("span", { hasText: "家里的 Mac" }).first().isVisible());
const remoteId = await page.evaluate(() => window.__mock.state.sessions[0].id);
await finishTurn(remoteId, "assistant", "来自另一台机器的回答");
await emit({ type: "done", sessionId: remoteId });
await waitFor(async () => !(await stopButton().isVisible()));

console.log("\n[11] 机器不在线：不能选它发消息");
await page.evaluate(() => {
  const m = window.__mock.state.machines.find((x) => x.id === "home-mac");
  m.online = false;
  m.status.error = "节点 20 秒内没有回应，判定为断线";
  window.__mock.emitNode({ id: m.id, name: m.name, online: false, message: m.status.error });
});
await page.getByRole("button", { name: "＋ 新会话" }).click();
await sleep(2600);
check("离线的机器不能选", await remoteChip().isDisabled());
check("本机仍然可选", !(await localChip().isDisabled()));

const failed = results.filter((r) => !r).length;
console.log(`\n${results.length - failed}/${results.length} 项通过${pageErrors.length ? `；页面错误 ${pageErrors.length} 条：` : ""}`);
for (const e of pageErrors.slice(0, 8)) console.log("  [page] " + e.slice(0, 300));
await browser.close();
process.exit(failed || pageErrors.length ? 1 : 0);
