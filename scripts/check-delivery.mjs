// Build GUI first. Isolated browser check; never sends a mutation to a live service.
import fs from "node:fs";
import http from "node:http";
import { spawn } from "node:child_process";
import { createRequire } from "node:module";
import path from "node:path";
import assert from "node:assert/strict";
import os from "node:os";
import { fileURLToPath } from "node:url";
const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const require = createRequire(repo + "/gui/package.json");
const { chromium } = require("@playwright/test");
const f = await import(repo + "/gui/test/mock-celeris/fixtures.ts");
const out = fs.mkdtempSync(path.join(os.tmpdir(), "celeris-delivery-gui-"));
let state = "reviewing";
const requests = [];
const sha = "bbbbbbbbbbbb";
const api = http.createServer((req, res) => {
  requests.push(req.method);
  if (req.method !== "GET") { res.writeHead(405); res.end(); return; }
  const p = new URL(req.url, "http://localhost").pathname.replace("/api/v1", "");
  if (p.endsWith("/stream")) { res.writeHead(200, {"content-type":"text/event-stream"}); res.write(": fixture\n\n"); return; }
  const values = {
    "/health": f.defaultHealth, "/org": {items:[]}, "/projects": {items:[]},
    "/config": {genres:[]}, "/daemon": {snapshot:{approvals_pending:0,reports:{unread_secretary:0,unread_total:0}}},
    "/inbox": {counts:{approvals:0,questions:0,drafts:0,attention:0},approvals:[],questions:[],drafts:[],attention:[]},
    "/tasks/fixture/changes": f.changesView({task_id:"fixture",delivery:{task_id:"fixture",project_id:"p1",repo_id:"r1",repo:"benchfs",branch:"celeris/fixture",base:"a".repeat(40),head:"b".repeat(40),default_branch:"main",department:"engineering",review_run:"review-run",worker_run:"worker-run",criterion_idx:1,decision:state==="reviewing"?null:state!=="blocked",state,detail:"実装完了と本番反映は別です。部署のレビュアーが判断し、ビルド・検証の完了後に人がデプロイします。",release:state==="ready"?sha:null,prepare_pid:null,notification:null}}),
    "/releases": {...f.defaultReleases,items:[f.releaseItem({sha12:sha,sha:"b".repeat(40),changes:f.releaseChanges()})]},
  };
  res.writeHead(p in values ? 200 : 404,{"content-type":"application/json"}); res.end(JSON.stringify(values[p] ?? {code:"not_found",detail:p}));
});
let gui, browser;
try {
  await new Promise((resolve,reject)=>{api.once("error",reject);api.listen(17991,"127.0.0.1",resolve)});
  const log = fs.openSync(path.join(out,"gui.log"),"w");
  gui=spawn(process.execPath,["server.js"],{cwd:repo+"/gui",env:{...process.env,NODE_ENV:"production",CELERIS_GUI_BIND:"127.0.0.1:17921",CELERIS_API_URL:"http://127.0.0.1:17991"},stdio:["ignore",log,log]});
  for(let i=0;i<100;i++){try{if((await fetch("http://127.0.0.1:17921/healthz")).ok)break}catch{} await new Promise(r=>setTimeout(r,100));}
  browser=await chromium.launch({headless:true}); const measurements=[];
  for(const width of [393,1440]) {
    const page=await browser.newPage({viewport:{width,height:900}}); const errors=[]; page.on("pageerror",e=>errors.push(e.message));
    await page.route("**/*",route=>{const u=new URL(route.request().url()); return u.hostname==="127.0.0.1" && u.port==="17921" && route.request().method()==="GET" ? route.continue():route.abort()});
    for(const s of ["reviewing","merge_queued","merging","preparing","ready","blocked"]) {
      state=s; const response=await page.goto("http://127.0.0.1:17921/tasks/fixture/changes"); assert.equal(response.status(),200);
      await page.getByTestId("delivery-status").waitFor();
      assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=window.innerWidth),true,`${s} ${width}: overflow`);
      assert.equal(await page.getByTestId("delivery-status").getByText("レビュー担当: engineering").count(),1);
      measurements.push({width,state:s,text:await page.getByTestId("delivery-status").innerText()});
      if(s==="ready") { await page.screenshot({path:path.join(out,`ready-${width}.png`),fullPage:true}); const link=page.getByRole("link",{name:/リリース .* を確認してデプロイ/}); assert.equal(await link.getAttribute("href"),`/releases#release-${sha}`); await link.click(); await page.locator(`#release-${sha}`).waitFor(); await page.locator(`#release-${sha} summary`).filter({hasText:/^昇格$/}).click(); assert.equal(await page.locator(`#release-${sha}`).getByRole("button").count()>0,true); }
    }
    assert.deepEqual(errors,[]); await page.close();
  }
  assert(requests.every(m=>m==="GET")); fs.writeFileSync(path.join(out,"measurements.json"),JSON.stringify(measurements,null,2)); console.log(JSON.stringify({ok:true,views:measurements.length,evidence:out}));
} finally { await browser?.close(); gui?.kill("SIGTERM"); api.closeAllConnections(); await new Promise(r=>api.close(r)); }
