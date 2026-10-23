// Deterministic checks of the embedded UI's inspection and label helpers.
// Run with: node tests/studio_diagram_ui.js [Pilot diagram JSON] [inspection HTML output]
// No browser session or user workspace is touched.
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const html = fs.readFileSync(path.join(__dirname, "../crates/ol_cli/src/studio_ui.html"), "utf8");
const elements = new Map();
function element(id) {
  if (!elements.has(id)) elements.set(id, { dataset: {}, innerHTML: "", textContent: "", hidden: false, open: false });
  return elements.get(id);
}
const context = vm.createContext({
  document: { getElementById: element },
  state: { diagramData: null, selection: null },
  DIAG: { ROW: 44, BOXH: 30, INW: 150, EQW: 280, OUTW: 170,
    X_IN: 16, X_EQ: 230, X_OUT: 580, PIN_PITCH: 16, PIN_MARGIN: 13 },
  selected: null,
});
for (const name of ["escapeHTML", "escapeAttr", "displayTypeKind", "typeName", "wrapText", "boxLabelLines", "boxSingleLabel",
  "expressionSourceText", "expressionDetailHTML", "renderDiagramExpression", "inspectDiagramExpression", "expressionDependencyTip", "eqShape",
  "eqWidth", "eqHeight", "operandGhostSet", "collapsedLocals", "computePositions"]) {
  const match = html.match(new RegExp("^function " + name + "\\([^]*?^}", "m"));
  assert.ok(match, "embedded helper exists: " + name);
  vm.runInContext(match[0], context);
}
vm.runInContext("function setSelection(id) { selected = id; state.selection = id; renderDiagramExpression(); }", context);
const bool = { kind: "Bool" };
const ref = (field, source = "sensor", type = bool) => ({
  path: field, type, source, source_type: { kind: "Named", name: "SensorAlias" },
  resolved_source_type: { kind: "Named", name: "EstimatedState" }, source_kind: "struct",
});
const eq = {
  id: "eq0", lhs: ["sensor_ok"], text: "sensor_ok = sensor.valid and sensor.attitude.valid and permit",
  symbol: { kind: "op", text: "AND" },
  expression_detail: {
    kind: "logical", operation: "Logical AND", has_projections: true,
    operands: ["sensor.valid", "sensor.attitude.valid", "permit"].map(text => ({ text, type: bool })),
    references: [ref("sensor.valid"), ref("sensor.attitude.valid"), {
      path: "permit", type: bool, source: "permit", source_type: bool, resolved_source_type: bool, source_kind: "value",
    }],
  },
};
const rendered = context.expressionDetailHTML(eq);
assert.match(rendered, /Logical AND/);
assert.match(rendered, /3 Boolean operands/);
assert.match(rendered, /sensor\.valid/);
assert.match(rendered, /sensor\.attitude\.valid/);
assert.match(rendered, /SensorAlias → EstimatedState \(struct\)/);
assert.match(rendered, /permit : bool/);
assert.match(rendered, /Full equation/);
assert.match(context.expressionDependencyTip(eq, "sensor"), /sensor\.attitude\.valid : bool/);
assert.match(context.expressionDependencyTip(eq, "sensor"), /Expression details/);
assert.equal(context.expressionDependencyTip(eq, "permit"), "permit");

// Long nested paths stay intact and can wrap in the inspection table.
const longPath = "sensor.nested." + "field_with_documented_name_".repeat(12);
const longEq = JSON.parse(JSON.stringify(eq));
longEq.expression_detail.operands[0].text = longPath;
longEq.expression_detail.references[0].path = longPath;
assert.ok(context.expressionDetailHTML(longEq).includes(longPath));
assert.match(html, /#diagram-expression-body[^]*?overflow: auto/);
assert.match(html, /#diagram-expression-body code[^]*?overflow-wrap: anywhere/);
const injected = JSON.parse(JSON.stringify(eq));
injected.expression_detail.operands[0].text = "sensor.<script>alert('x')<\/script>";
assert.ok(!context.expressionDetailHTML(injected).includes("<script>"));
assert.match(context.expressionDetailHTML(injected), /&lt;script&gt;/);

// Defaults reveal the first logical expression with projections. Choosing
// another equation updates the same panel and preserves its disclosure state.
const scalar = { id: "eq1", lhs: ["ready"], text: "ready = permit", symbol: null };
context.state.diagramData = { node: "Pilot", equations: [scalar, eq] };
context.renderDiagramExpression();
assert.equal(element("diagram-expressions").dataset.equation, "eq0");
assert.match(element("diagram-expression-summary").textContent, /sensor_ok · Logical AND · 3 operands/);
context.inspectDiagramExpression("eq1");
assert.equal(context.selected, "eq1");
assert.equal(element("diagram-expressions").open, true);
assert.equal(element("diagram-expression-body").innerHTML, "<pre>ready = permit</pre>");
element("diagram-expressions").open = false;
context.renderDiagramExpression();
assert.equal(element("diagram-expressions").open, false);
const originalBody = element("diagram-expression-body").innerHTML;
element("diagram-expression-body").innerHTML = "inspection scroll preserved";
context.renderDiagramExpression();
assert.equal(element("diagram-expression-body").innerHTML, "inspection scroll preserved");
element("diagram-expression-body").innerHTML = originalBody;
context.state.selection = null;
context.state.diagramData = { node: "Empty", equations: [] };
context.renderDiagramExpression();
assert.equal(element("diagram-expressions").hidden, true);

// Gates retain distinct logical and bitwise shapes; the inspection labels
// preserve the API's explicit operation kind.
assert.equal(context.eqShape(eq), "and");
const bitwise = JSON.parse(JSON.stringify(eq));
bitwise.symbol.text = "&";
bitwise.expression_detail.kind = "bitwise";
bitwise.expression_detail.operation = "Bitwise AND (&)";
assert.equal(context.eqShape(bitwise), null);
assert.match(context.expressionDetailHTML(bitwise), /Bitwise AND \(&amp;\)/);
assert.ok(!context.expressionDetailHTML(bitwise).includes("Boolean operands"));

// Box labels fit the available width, with complete names retained in titles.
const label = { label: "sensor : EXPEstimatedState", sym: false };
const clipped = context.boxSingleLabel(label, { w: 150 });
assert.ok(clipped.endsWith("…"));
assert.ok(clipped.length * 6.6 <= 150 - 16);
assert.equal(context.boxSingleLabel(label, { w: 350 }), label.label);
assert.equal(context.boxSingleLabel(label, { w: 12 }), "…");
assert.match(html, /svg \+= '<title>' \+ escapeHTML\(it\.label\)/);
const lines = context.boxLabelLines(label, { w: 100 });
assert.ok(lines.length > 1);
assert.ok(lines.every(line => line.length * 6.6 <= 100 - 16));

// Inspection metadata has no effect on gate dimensions, saved positions,
// ports, or the graph editor's collapsed locals.
const diagram = {
  inputs: [{ name: "sensor", type: { kind: "Named", name: "SensorAlias" } }],
  outputs: [{ name: "sensor_ok", type: bool }], locals: [], ghosts: [], constructs: [],
  equations: [{ ...eq, inputs: [{ name: "sensor", bound: true }] }],
  wires: [{ from: "sensor", to: "eq0", to_port: 0 }, { from: "eq0", to: "sensor_ok" }],
  positions: { eq0: { x: 333, y: 120, w: 63, h: 40 } },
};
const withDetails = JSON.stringify(context.computePositions(diagram));
const beforeDiagram = JSON.stringify(diagram);
const plainDiagram = JSON.parse(beforeDiagram);
delete plainDiagram.equations[0].expression_detail;
assert.equal(JSON.stringify(context.computePositions(plainDiagram)), withDetails);
assert.equal(JSON.stringify(diagram), beforeDiagram);
console.log("studio_diagram_ui: checks passed (operands, nested paths, aliases, selection, escaping, clipping, inspectability, preserved layout)");

// --- Newer canvas helpers: wire identities and overlap geometry -------------
for (const name of ["wireKey", "rectsOverlap", "overlapGuardOn"]) {
  const match = html.match(new RegExp("^function " + name + "\\([^]*?^}", "m"));
  assert.ok(match, "embedded helper exists: " + name);
  vm.runInContext(match[0], context);
}
assert.equal(context.wireKey("x", "eq0", 1), "x|eq0|1");
assert.equal(context.wireKey("eq0", "y", undefined), "eq0|y|");
assert.ok(context.rectsOverlap({ x: 0, y: 0, w: 10, h: 10 }, { x: 5, y: 5, w: 10, h: 10 }, 0));
assert.ok(!context.rectsOverlap({ x: 0, y: 0, w: 10, h: 10 }, { x: 12, y: 0, w: 10, h: 10 }, 0), "clear horizontally");
assert.ok(!context.rectsOverlap({ x: 0, y: 0, w: 10, h: 10 }, { x: 12, y: 0, w: 10, h: 10 }, 2), "gap margin respected");
assert.ok(context.rectsOverlap({ x: 0, y: 0, w: 10, h: 10 }, { x: 11, y: 0, w: 10, h: 10 }, 2), "gap margin closes in");
element("block-overlaps").checked = true; // a real checkbox defaults the guard on
assert.equal(context.overlapGuardOn(), true, "guard defaults to on");
element("block-overlaps").checked = false;
assert.equal(context.overlapGuardOn(), false, "guard follows the View toggle");
console.log("studio_diagram_ui: wire keys and overlap geometry checked");

if (process.argv[2]) {
  const actual = JSON.parse(fs.readFileSync(process.argv[2], "utf8"));
  const sensorEq = actual.equations.find(e => e.lhs.includes("sensor_ok"));
  assert.ok(sensorEq, "actual Pilot sensor_ok equation");
  assert.equal(sensorEq.expression_detail.operation, "Logical AND");
  assert.equal(sensorEq.expression_detail.operands.length, 17);
  assert.ok(sensorEq.expression_detail.operands.every(o => o.type.kind === "Bool"));
  context.state.diagramData = actual;
  context.state.selection = sensorEq.id;
  context.renderDiagramExpression();
  const actualHTML = element("diagram-expression-body").innerHTML;
  assert.match(actualHTML, /17 Boolean operands/);
  for (const field of ["sensor.valid", "sensor.attitude_valid", "sensor.rates_valid", "sensor.sample_time_ms", "sensor.roll_rate_rad_s"]) {
    assert.ok(actualHTML.includes(field), "actual field rendered: " + field);
  }
  assert.match(actualHTML, /EXPEstimatedState \(struct\)/);
  assert.ok(actualHTML.includes(sensorEq.body.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;")));
  console.log("studio_diagram_ui: actual Pilot API metadata rendered and checked (17 Boolean operands, validity/time/range fields, named struct)");
  if (process.argv[3]) {
    const css = html.slice(html.indexOf("  #diagram-expressions {"), html.indexOf("  svg .box {"));
    const heading = context.escapeHTML(actual.node + " · " + element("diagram-expression-summary").textContent);
    fs.writeFileSync(process.argv[3], '<!doctype html><html lang="en"><meta charset="utf-8"><title>Pilot expression inspection</title>' +
      '<style>body{font:13px system-ui;margin:20px;max-width:1000px}.build-note{color:#555}' + css +
      '#diagram-expression-body{max-height:none}</style><h1 style="font-size:18px">' + heading +
      '</h1><p>Deterministic rendering of actual diagram API data with the embedded Studio inspection helpers.</p>' +
      '<div id="diagram-expression-body">' + actualHTML + '</div></html>');
  }
}
