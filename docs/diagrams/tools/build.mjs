import { fileURLToPath } from "node:url";
import { C, Diagram } from "./lib.mjs";

// Writes the .excalidraw source files. Default output: docs/diagrams.
const OUT = process.argv[2] ?? fileURLToPath(new URL("..", import.meta.url));
const all = [];

// 01 — System context
{
  const d = new Diagram("01-system-context");
  d.title(0, 0, "SaveSpace: system context");
  d.box("user", 40, 120, 220, 90, "Person\n(web, iOS, Android)", { bg: C.blue });
  d.box("platform", 420, 90, 320, 150, "SaveSpace platform\napi · worker · PostgreSQL", { bg: C.violet, size: 20 });
  d.box("agg", 900, 40, 280, 80, "Aggregators\nPlaid (primary) · MX", { bg: C.orange });
  d.box("banks", 1400, 40, 240, 80, "Banks and\ncard issuers", { bg: C.gray });
  d.box("claude", 900, 170, 280, 80, "Anthropic Claude API\n(rephrase only)", { bg: C.teal });
  d.box("delivery", 900, 300, 280, 80, "Delivery\nAPNs · FCM · Amazon SES", { bg: C.yellow });
  d.box("kms", 420, 320, 320, 80, "AWS KMS · Secrets Manager", { bg: C.gray });
  d.arrow("user", "platform", "HTTPS");
  d.arrow("platform", "agg", "API calls", { sides: ["r", "l"], offset: [[0, -70], [0, -25]] });
  d.arrow("agg", "platform", "webhooks", { sides: ["l", "r"], offset: [[0, 25], [0, -15]], dashed: true });
  d.arrow("agg", "banks", "OAuth / bank APIs", { both: true });
  d.arrow("platform", "claude", "fact sets", { sides: ["r", "l"], offset: [[0, 30], [0, 0]] });
  d.arrow("platform", "delivery", "", { sides: ["r", "l"], offset: [[0, 60], [0, 0]] });
  d.arrow("delivery", "user", "", { sides: ["b", "b"], via: [[1040, 450], [150, 450]] });
  d.text(470, 462, "push · email (respects quiet hours)", { size: 16 });
  d.arrow("platform", "kms", "envelope encryption");
  all.push(d);
}

// 02 — Container architecture
{
  const d = new Diagram("02-architecture");
  d.title(0, 0, "SaveSpace: container architecture (modular monolith)");
  d.zone("z-clients", 0, 60, 1000, 160, "Clients");
  d.box("web", 30, 100, 300, 90, "Web (phase 1)\nReact + TS · core (WASM)", { bg: C.blue });
  d.box("ios", 360, 100, 300, 90, "iOS (phase 2)\nSwiftUI · core (UniFFI)", { bg: C.blue });
  d.box("android", 690, 100, 280, 90, "Android (phase 3)\nCompose · core (UniFFI)", { bg: C.blue, dashed: true });
  d.box("cdn", 30, 260, 300, 70, "CloudFront\n(static web assets)", { bg: C.gray });
  d.box("alb", 360, 260, 610, 70, "ALB + AWS WAF (TLS 1.3)", { bg: C.gray });
  d.zone("z-ecs", 0, 370, 1000, 260, "ECS Fargate · one Rust workspace");
  d.box("api", 30, 420, 440, 180, "api (axum)\n/v1 REST · OpenAPI (utoipa)\nauth · idempotency keys\nwebhook ingress · SSE", { bg: C.violet });
  d.box("worker", 530, 420, 440, 180, "worker\nsync · categorize · recurring\ninsights · notify\nscheduler", { bg: C.violet });
  d.zone("z-data", 0, 670, 1000, 170, "Data");
  d.box("redis", 30, 720, 340, 90, "Redis (ElastiCache)\nrate limits · cache", { bg: C.red });
  d.box("pg", 410, 720, 560, 90, "PostgreSQL (RDS Multi-AZ, PITR)\ndomain tables · RLS · job queue · outbox", { bg: C.green });
  d.zone("z-platform", 1080, 60, 340, 290, "Platform services");
  d.box("kms", 1110, 105, 280, 60, "KMS · Secrets Manager", { bg: C.white });
  d.box("otel", 1110, 180, 280, 60, "OpenTelemetry → Grafana", { bg: C.white });
  d.box("flags", 1110, 255, 280, 60, "Feature flags", { bg: C.white });
  d.zone("z-ext", 1080, 370, 340, 470, "External");
  d.box("plaid", 1110, 420, 280, 80, "Plaid / MX", { bg: C.orange });
  d.box("claude", 1110, 540, 280, 80, "Claude API", { bg: C.teal });
  d.box("push", 1110, 660, 280, 80, "APNs · FCM · SES", { bg: C.yellow });

  d.arrow("web", "cdn", "", { sides: ["b", "t"], offset: [[-60, 0], [-60, 0]] });
  d.arrow("web", "alb", "", { sides: ["b", "t"], offset: [[60, 0], [-245, 0]] });
  d.arrow("ios", "alb", "", { sides: ["b", "t"], offset: [[0, 0], [-155, 0]] });
  d.arrow("android", "alb", "", { sides: ["b", "t"], offset: [[0, 0], [165, 0]], dashed: true });
  d.arrow("alb", "api", "", { sides: ["b", "t"], offset: [[-245, 0], [170, 0]] });
  d.arrow("api", "redis", "", { sides: ["b", "t"], offset: [[-50, 0], [0, 0]] });
  d.arrow("api", "pg", "sqlx", { sides: ["b", "t"], offset: [[150, 0], [-190, 0]] });
  d.arrow("worker", "pg", "claim jobs\n(SKIP LOCKED)", { sides: ["b", "t"], offset: [[0, 0], [60, 0]] });
  d.arrow("worker", "plaid", "sync", { sides: ["r", "l"], offset: [[0, -40], [0, 20]] });
  d.arrow("worker", "claude", "rephrase", { sides: ["r", "l"], offset: [[0, 0], [0, 0]] });
  d.arrow("worker", "push", "send", { sides: ["r", "l"], offset: [[0, 40], [0, 0]] });
  d.arrow("plaid", "alb", "webhooks", { sides: ["l", "r"], offset: [[0, -20], [0, 0]], via: [[1040, 440], [1040, 295]], dashed: true });
  all.push(d);
}

// 03 — Shared Rust core
{
  const d = new Diagram("03-shared-core");
  d.title(0, 0, "SaveSpace: one Rust core on every platform");
  d.box("server", 380, 60, 360, 90, "api + worker\n(native Rust dependency)", { bg: C.green });
  d.box("core", 380, 250, 360, 160, "savespace-core (Rust crate)\nmoney · budget math\nbaselines · insight formatting\nlocale + currency", { bg: C.violet, size: 20 });
  d.box("tests", 860, 250, 300, 160, "One test suite\nproptest · golden files\nsame output on\nevery platform", { bg: C.yellow });
  d.box("wasm", 40, 500, 260, 70, "wasm-bindgen\n+ wasm-pack", { bg: C.gray });
  d.box("uswift", 430, 500, 260, 70, "UniFFI → Swift\n(XCFramework)", { bg: C.gray });
  d.box("ukotlin", 820, 500, 260, 70, "UniFFI → Kotlin\n(AAR)", { bg: C.gray });
  d.box("webapp", 40, 640, 260, 80, "Web app\n(React + TS)", { bg: C.blue });
  d.box("iosapp", 430, 640, 260, 80, "iOS app\n(SwiftUI)", { bg: C.blue });
  d.box("andapp", 820, 640, 260, 80, "Android app\n(Compose)", { bg: C.blue, dashed: true });
  d.arrow("core", "server", "cargo dependency", { sides: ["t", "b"] });
  d.arrow("core", "tests", "", { dashed: true });
  d.arrow("core", "wasm", "", { sides: ["b", "t"], offset: [[-120, 0], [0, 0]] });
  d.arrow("core", "uswift", "", { sides: ["b", "t"], offset: [[0, 0], [0, 0]] });
  d.arrow("core", "ukotlin", "", { sides: ["b", "t"], offset: [[120, 0], [0, 0]] });
  d.arrow("wasm", "webapp", "");
  d.arrow("uswift", "iosapp", "");
  d.arrow("ukotlin", "andapp", "");
  all.push(d);
}

// 04 — Data model
{
  const d = new Diagram("04-data-model");
  d.title(0, 0, "SaveSpace: core data model (PostgreSQL)");
  d.text(0, 40, "Money = BIGINT minor units + ISO 4217 code. Every row has created_at / updated_at (UTC).", { size: 16, color: "#495057" });
  const colX = [0, 400, 800, 1200];
  const colY = [90, 90, 90, 90];
  const size = 15;
  const entity = (id, col, name, fields, bg = C.white) => {
    const label = `${name}\n${fields.map((f) => `· ${f}`).join("\n")}`;
    const h = Math.ceil((fields.length + 1) * size * 1.25) + 26;
    d.box(id, colX[col], colY[col], 330, h, label, { bg, size, align: "left", valign: "top" });
    colY[col] += h + 50;
  };
  entity("users", 0, "users", ["id (uuid)", "email", "time_zone", "calm_mode", "quiet_hours"], C.blue);
  entity("check_ins", 0, "check_ins", ["user_id", "mood (1–5)", "note (encrypted)"], C.yellow);
  entity("insights", 0, "insights", ["user_id", "kind · rule_id · rule_version", "facts (jsonb)", "text · render_mode", "confidence · expires_at"], C.teal);
  entity("insight_feedback", 0, "insight_feedback", ["insight_id", "verdict (wrong | not_helpful | helpful)", "note"], C.teal);

  entity("members", 1, "household_members", ["household_id", "user_id", "role (owner | partner)"], C.blue);
  entity("households", 1, "households", ["id", "name"], C.blue);
  entity("connections", 1, "connections", ["household_id", "provider · item_id", "access_token_enc", "sync_cursor · status"], C.orange);
  entity("accounts", 1, "accounts", ["connection_id", "type · subtype · mask", "currency · hidden"], C.orange);
  entity("balances", 1, "balances", ["account_id · as_of", "current_minor", "available_minor"], C.orange);

  entity("raw", 2, "transactions_raw (append-only)", ["account_id", "provider_txn_id · version", "payload (jsonb)", "removed_at"], C.gray);
  entity("txns", 2, "transactions (enriched)", ["raw_id · account_id", "posted_on · status", "amount_minor · currency", "merchant_id · category_id", "recurring_series_id"], C.violet);
  entity("overrides", 2, "user_overrides", ["transaction_id", "field · value", "updated_by"], C.violet);

  entity("categories", 3, "categories", ["household_id", "name · group", "kind (fixed | flex | non_monthly)"], C.green);
  entity("budgets", 3, "budgets", ["household_id · month", "category_id", "planned_minor"], C.green);
  entity("rules", 3, "rules", ["household_id · priority", "match (jsonb)", "set (jsonb)"], C.green);
  entity("recurring", 3, "recurring_series", ["merchant_id · cadence", "expected_amount_minor", "next_expected_on", "confidence"], C.green);

  const rel = (a, b, label = "1:N", o = {}) => d.arrow(a, b, label, { labelSize: 14, ...o });
  rel("users", "check_ins");
  rel("users", "insights", "1:N", { sides: ["l", "l"], via: [[-40, 160], [-40, 0]] });
  rel("insights", "insight_feedback");
  rel("users", "members");
  rel("households", "members", "1:N", { sides: ["t", "b"] });
  rel("households", "connections", "1:N", { sides: ["b", "t"] });
  rel("connections", "accounts");
  rel("accounts", "balances");
  rel("accounts", "raw");
  rel("raw", "txns", "1:1");
  rel("txns", "overrides");
  rel("categories", "txns", "1:N", { sides: ["l", "r"] });
  rel("recurring", "txns", "1:N", { sides: ["l", "r"], offset: [[0, 0], [0, 40]] });
  rel("categories", "budgets");
  all.push(d);
}
// Fix the users→insights path now that the boxes exist.
{
  const d = all[3];
  const users = d.byId.get("users");
  const ins = d.byId.get("insights");
  const a = d.els.find((e) => e.type === "arrow" && e.startBinding?.elementId === "users" && e.endBinding?.elementId === "insights");
  const y1 = users.y + users.height / 2;
  const y2 = ins.y + ins.height / 2;
  a.x = users.x;
  a.y = y1;
  a.points = [[0, 0], [-40, 0], [-40, y2 - y1], [0, y2 - y1]];
  a.width = 40;
  a.height = y2 - y1;
  const lbl = d.byId.get(`${a.id}-label`);
  if (lbl) {
    lbl.x = users.x - 40 - lbl.width / 2;
    lbl.y = (y1 + y2) / 2 - lbl.height / 2;
  }
}

// 05 — Account link sequence
{
  const d = new Diagram("05-account-link-sequence");
  d.title(0, 0, "SaveSpace: link a bank account (sequence)");
  const P = [
    ["user", "Person", C.blue],
    ["web", "Web app", C.blue],
    ["api", "api", C.violet],
    ["pg", "PostgreSQL", C.green],
    ["worker", "worker", C.violet],
    ["plaid", "Plaid", C.orange],
  ];
  const X = {};
  const top = 60;
  const msgs = [
    ["user", "web", "1. Select “Connect account”"],
    ["web", "api", "2. POST /v1/link_tokens"],
    ["api", "plaid", "3. /link/token/create"],
    ["plaid", "api", "4. link_token", true],
    ["api", "web", "5. link_token", true],
    ["web", "plaid", "6. Open Plaid Link. Person signs in to the bank."],
    ["plaid", "web", "7. public_token", true],
    ["web", "api", "8. POST /v1/connections\n(Idempotency-Key)"],
    ["api", "plaid", "9. /item/public_token/exchange"],
    ["plaid", "api", "10. access_token · item_id", true],
    ["api", "pg", "11. One tx: connection +\nencrypted token + sync job"],
    ["api", "web", "12. 201 {status: syncing}", true],
    ["worker", "pg", "13. Claim job\n(SKIP LOCKED)"],
    ["worker", "plaid", "14. /transactions/sync"],
    ["plaid", "worker", "15. added · modified ·\nremoved · next_cursor", true],
    ["worker", "pg", "16. Upsert rows + cursor\n+ outbox event"],
    ["pg", "api", "17. outbox: connection.ready", true],
    ["api", "web", "18. SSE: connection.ready", true],
  ];
  const rowH = 62;
  const bottom = top + 110 + msgs.length * rowH;
  P.forEach(([id, name, bg], i) => {
    const cx = 110 + i * 260;
    X[id] = cx;
    d.box(id, cx - 100, top, 200, 56, name, { bg });
    d.line(cx, top + 56, cx, bottom, { dashed: true });
  });
  msgs.forEach(([a, b, label, reply], i) => {
    const y = top + 130 + i * rowH;
    const dir = X[b] > X[a] ? 1 : -1;
    d.freeArrow(X[a] + 4 * dir, y, X[b] - 4 * dir, y, label, { dashed: !!reply, labelSize: 15 });
  });
  d.box("note", 110, bottom + 30, 1300, 70,
    "The access token never leaves the server. The api encrypts it with a per-household data key (KMS envelope encryption).",
    { bg: C.yellow, size: 16 });
  all.push(d);
}

// 06 — Transaction sync pipeline
{
  const d = new Diagram("06-transaction-sync");
  d.title(0, 0, "SaveSpace: transaction sync pipeline");
  d.box("wh", 0, 80, 240, 100, "Plaid webhook\nSYNC_UPDATES_\nAVAILABLE", { bg: C.orange });
  d.box("ingress", 310, 80, 250, 100, "Webhook ingress\nverify JWT signature\nstore raw · return 200", { bg: C.violet });
  d.box("enqueue", 630, 80, 250, 100, "Enqueue sync job\ndedupe key: item_id", { bg: C.green });
  d.box("cron", 630, 250, 250, 80, "Scheduler\nfallback poll every 6 h", { bg: C.gray, dashed: true });
  d.box("sync", 960, 80, 290, 100, "Sync loop\n/transactions/sync + cursor\nrepeat while has_more", { bg: C.violet });
  d.box("apply", 960, 250, 290, 160, "Apply in one DB tx\nadded → insert raw\nmodified → new raw version\nremoved → set removed_at\nsave next_cursor", { bg: C.violet });
  d.box("enrich", 960, 480, 290, 110, "Enrich\nnormalize merchant\nlink pending → posted", { bg: C.blue });
  d.box("cat", 620, 480, 280, 110, "Categorize\nuser rules > model >\nprovider category", { bg: C.blue });
  d.box("rec", 300, 480, 260, 110, "Recurring detection\ncadence + amount band", { bg: C.blue });
  d.box("outbox", 0, 480, 240, 110, "Outbox event\ntransactions.changed", { bg: C.green });
  d.box("ins", 0, 670, 240, 80, "Insight job\n(see diagram 07)", { bg: C.teal });
  d.box("budget", 300, 670, 260, 80, "Budget recalculation", { bg: C.teal });
  d.box("note", 0, 250, 560, 160,
    "Idempotency rules\n· The webhook handler is safe to replay.\n· The cursor and the rows change in one tx.\n· Raw rows are append-only.\n· A job retry gives the same result.",
    { bg: C.yellow, size: 17, align: "left", valign: "top" });
  d.arrow("wh", "ingress");
  d.arrow("ingress", "enqueue");
  d.arrow("cron", "enqueue", "", { dashed: true });
  d.arrow("enqueue", "sync");
  d.arrow("sync", "apply");
  d.arrow("apply", "enrich");
  d.arrow("enrich", "cat");
  d.arrow("cat", "rec");
  d.arrow("rec", "outbox");
  d.arrow("outbox", "ins", "", { sides: ["b", "t"], offset: [[-60, 0], [-60, 0]] });
  d.arrow("outbox", "budget", "", { sides: ["b", "t"], offset: [[60, 0], [0, 0]], via: [[180, 630], [430, 630]] });
  all.push(d);
}

// 07 — Insight pipeline
{
  const d = new Diagram("07-insight-pipeline");
  d.title(0, 0, "SaveSpace: insight pipeline");
  d.zone("z-in", 0, 60, 260, 470, "Inputs");
  const ins = ["Transactions", "Budgets", "Recurring series", "Check-ins", "Pay schedule"];
  ins.forEach((t, i) => d.box(`in${i}`, 20, 110 + i * 80, 220, 60, t, { bg: C.white }));
  d.box("facts", 340, 230, 300, 120, "Fact builder\nrolling median + MAD\npay-cycle windows", { bg: C.violet });
  d.box("rules", 720, 230, 260, 120, "Rule engine\nversioned insight rules\nfact preconditions", { bg: C.violet });
  d.box("score", 1120, 230, 260, 120, "Scoring\nconfidence ≥ threshold\nrelevance · novelty", { bg: C.violet });
  d.box("policy", 1120, 420, 260, 140, "Policy filter\ndedupe · rate limit\nmax 3 per view\nquiet hours · calm mode", { bg: C.blue });
  d.box("tone", 720, 420, 260, 140, "Tone guard\nframing rules\nno shame · no false\nreassurance", { bg: C.blue });
  d.box("render", 720, 640, 260, 90, "Template render\n(always runs)", { bg: C.green });
  d.box("llm", 1120, 640, 260, 90, "LLM rephrase (optional)\nClaude · facts only", { bg: C.teal, dashed: true });
  d.box("validate", 1120, 800, 260, 130, "Validator\nsame numbers as facts\nno banned words\nlength cap", { bg: C.red });
  d.box("store", 720, 820, 260, 90, "Store insight\ntext + facts + mode", { bg: C.green });
  d.box("deliver", 340, 820, 300, 90, "Deliver\nfeed · weekly reflection · push", { bg: C.green });
  d.box("feedback", 0, 820, 260, 90, "Person feedback\nwrong · not helpful", { bg: C.yellow });
  ins.forEach((_, i) => d.arrow(`in${i}`, "facts", "", { sides: ["r", "l"] }));
  d.arrow("facts", "rules");
  d.arrow("rules", "score");
  d.arrow("score", "policy");
  d.arrow("policy", "tone");
  d.arrow("tone", "render");
  d.arrow("render", "llm", "if enabled");
  d.arrow("llm", "validate");
  d.arrow("validate", "store", "pass:\nLLM text", { sides: ["l", "r"] });
  d.arrow("render", "store", "fail or off:\ntemplate text", { dashed: true });
  d.arrow("store", "deliver");
  d.arrow("deliver", "feedback");
  d.arrow("feedback", "rules", "tune rules + thresholds", {
    sides: ["t", "l"], dashed: true, offset: [[0, 0], [0, 40]], via: [[130, 600], [680, 600], [680, 330]], labelAt: [400, 620],
  });
  all.push(d);
}

for (const d of all) console.log(d.save(OUT));
