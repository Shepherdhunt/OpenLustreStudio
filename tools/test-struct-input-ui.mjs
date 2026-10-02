// Deterministic DOM/function tests of the embedded UI; no browser screenshot is claimed.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const html = fs.readFileSync(path.join(root, 'crates/ol_cli/src/studio_ui.html'), 'utf8');
const script = html.match(/<script>([\s\S]*?)<\/script>/)[1];
new vm.Script(script); // entire shipped script must parse
function extract(name) {
  const start = script.indexOf(`function ${name}(`);
  assert(start >= 0, name);
  const tail = script.slice(start), line = tail.split('\n')[0];
  if (line.endsWith('}')) return line;
  const end = tail.indexOf('\n}');
  assert(end >= 0, name);
  return tail.slice(0, end + 2);
}
class Element {
  constructor(tag) { this.tag = tag; this.children = []; this.events = {}; this.dataset = {}; this.attributes = {}; this.style = {}; this.value = ''; this.textContent = ''; this.classes = new Set(); this.classList = { toggle: (n, yes) => yes ? this.classes.add(n) : this.classes.delete(n) }; }
  set innerHTML(value) { this.html = value; this.children = []; }
  get innerHTML() { return this.html || ''; }
  appendChild(child) { this.children.push(child); return child; }
  addEventListener(name, fn) { this.events[name] = fn; }
  setAttribute(name, value) { this.attributes[name] = value; }
}
const elements = new Map();
const document = { createElement: tag => new Element(tag), getElementById: id => {
  if (!elements.has(id)) elements.set(id, new Element('div'));
  return elements.get(id);
} };
const state = { sim: { values: {}, session: { root: 'Test', signals: [] } } };
let steps = 0;
const ctx = vm.createContext({ document, state, simStep: () => steps++ });
for (const name of ['escapeHTML', 'displayTypeKind', 'typeName', 'typeKind', 'defaultForKind', 'validateValue', 'onWatchInput', 'buildWatchTable', 'kindOfTypeStr', 'simSignals', 'simPlumbing', 'simResolvedSchema', 'simSchemaHTML', 'simValidateJSON', 'simValidateInput', 'simInitialInput', 'onSimWatchInput', 'simBuildWatchTable']) vm.runInContext(extract(name), ctx);
const scalar = type => ({ kind: 'scalar', type });
const sample = { kind: 'struct', type: 'Sensor', name: 'Sensor', fields: [
  { name: 'valid', type: 'bool', schema: scalar('bool') },
  { name: 'timestamp', type: 'int64', schema: scalar('int64') },
  { name: 'nested', type: 'Flags[2]', schema: { kind: 'array', type: 'Flags[2]', len: 2, elem: { kind: 'alias', type: 'Flags', target: { kind: 'struct', type: 'Inner', fields: [{ name: 'healthy', type: 'bool', schema: scalar('bool') }] } } } }
] };
const value = { valid: false, timestamp: 0, nested: [{ healthy: false }, { healthy: false }] };
const signal = { name: 'sensor', kind: 'input', type: 'Sensor', schema: sample, default_value: value, placeholder_note: 'Generated placeholder: zero / false; enter simulation values.' };
assert.equal(ctx.simValidateInput(signal, JSON.stringify(value)).ok, true);
assert.match(ctx.simValidateInput(signal, '0').msg, /expected struct Sensor/);
assert.match(ctx.simValidateInput(signal, '').msg, /enter a value/);
assert.match(ctx.simValidateInput(signal, '{}').msg, /missing field valid/);
assert.match(ctx.simValidateInput(signal, JSON.stringify({ ...value, extra: 1 })).msg, /extra field extra/);
assert.match(ctx.simValidateInput(signal, JSON.stringify({ ...value, valid: 'false' })).msg, /sensor.valid: expected bool/);
assert.match(ctx.simValidateInput(signal, JSON.stringify({ ...value, nested: [{ healthy: 'true' }, { healthy: false }] })).msg, /sensor.nested\[0\].healthy: expected bool/);
assert.match(ctx.simValidateInput(signal, JSON.stringify({ ...value, nested: [] })).msg, /expected 2 elements/);
const exact = '{"valid":false,"timestamp":9007199254740993,"nested":[{"healthy":false},{"healthy":false}]}';
assert.equal(ctx.simValidateInput(signal, exact).norm, exact, 'numeric text must never be reserialized through JS Number');
const exactMax = exact.replace('9007199254740993', '9223372036854775807');
assert.equal(ctx.simValidateInput(signal, exactMax).norm, exactMax, 'supported i64 maximum must not be rejected due to JS rounding');
assert.equal(ctx.simValidateInput({ type: 'int64', schema: scalar('int64') }, '9223372036854775807').ok, true);
assert.equal(ctx.simValidateInput({ type: 'int64', schema: scalar('int64') }, '9223372036854775808').ok, false);
assert.equal(ctx.simValidateInput({ type: 'float32', schema: scalar('float32') }, '1e100').ok, false);
assert.equal(ctx.simValidateInput(signal, '{valid=false;timestamp=0;nested=[{healthy=false};{healthy=false}]}').ok, true, 'legacy syntax remains server validated');
const arraySignal = { name: 'samples', type: 'Sensor[2]', schema: { kind: 'array', type: 'Sensor[2]', len: 2, elem: sample } };
assert.equal(ctx.simValidateInput(arraySignal, '[{valid=false;timestamp=0;nested=[{healthy=false};{healthy=false}]};{valid=true;timestamp=1;nested=[{healthy=false};{healthy=true}]}]').ok, true, 'legacy array-of-struct trace remains editable');
assert.equal(ctx.simValidateInput({ type: 'int32[2]', schema: { kind: 'array', type: 'int32[2]', len: 2, elem: scalar('int32') } }, '[1;2]').norm, '[1;2]');
const inspection = ctx.simSchemaHTML(sample, 'sensor');
assert.match(inspection, /Sensor \(struct\)/);
assert.match(inspection, /sensor.nested\[index\].healthy : bool/);
assert.match(inspection, /Flags \(alias\)/);
assert.equal(ctx.displayTypeKind('Record'), 'struct');
assert.equal(ctx.typeName({ kind: 'Record' }), 'struct');
assert.equal(ctx.simValidateInput({ type: 'char', schema: scalar('char') }, '255').ok, true);
assert.equal(ctx.simValidateInput({ type: 'char', schema: scalar('char') }, '256').ok, false);
assert.equal(ctx.simValidateJSON(scalar('char'), 65, 'letter'), '');
assert.match(ctx.simValidateJSON(scalar('char'), 'A', 'letter'), /character byte/);
let deepAlias = sample;
for (let i = 0; i < 40; i++) deepAlias = { kind: 'alias', type: 'Alias' + i, target: deepAlias };
assert.equal(ctx.simResolvedSchema(deepAlias).kind, 'struct');
assert.equal(ctx.simValidateInput({ ...signal, schema: deepAlias }, JSON.stringify(value)).ok, true);
const enumSignal = { name: 'mode', kind: 'input', type: 'Mode', schema: { kind: 'enum', type: 'Mode', variants: ['ARMED', 'DISARMED'] }, default_text: 'DISARMED' };
const requiredEnum = { name: 'action', kind: 'input', type: 'Action', schema: { kind: 'enum', type: 'Action', variants: ['FIRE', 'LOAD'] }, default_error: 'Action requires a user-selected variant' };
assert.equal(ctx.simInitialInput(requiredEnum), '');
assert.equal(ctx.simValidateInput(requiredEnum, '').ok, false);
state.sim.session.signals = [signal, enumSignal, requiredEnum, { name: 'output', kind: 'output', type: 'Sensor', schema: sample }];
ctx.simBuildWatchTable();
const rows = document.getElementById('sim-watch-body').children;
assert.equal(rows.length, 4);
const input = rows[0].children[1].children[0];
assert.equal(input.tag, 'textarea');
assert.equal(JSON.parse(input.value).valid, false);
assert.equal(input.attributes['aria-label'], 'sensor input, Sensor');
assert.equal(input.attributes['aria-invalid'], 'false');
assert.match(rows[0].children[0].children[0].innerHTML, /Type \/ fields/);
assert.match(rows[0].children[1].children[1].textContent, /placeholder/);
assert.equal(rows[1].children[1].children[0].tag, 'select');
assert.equal(rows[1].children[1].children[0].value, 'DISARMED');
assert.equal(rows[2].children[1].children[0].value, '', 'active first enum variant must not be selected implicitly');
input.value = '{}'; input.events.input();
assert.equal(input.attributes['aria-invalid'], 'true');
assert.match(input.inputError.textContent, /missing field/);
input.value = JSON.stringify(value); input.events.input();
assert.equal(input.inputError.textContent, '');
assert.equal(input.attributes['aria-invalid'], 'false');
input.events.keydown({ key: 'Enter', preventDefault() {} });
assert.equal(steps, 0, 'plain Enter must edit multiline JSON');
input.events.keydown({ key: 'Enter', ctrlKey: true, preventDefault() {} });
assert.equal(steps, 1);
state.sim.values.sensor = JSON.stringify({ ...value, valid: true });
ctx.simBuildWatchTable();
assert.equal(JSON.parse(document.getElementById('sim-watch-body').children[0].children[1].children[0].value).valid, true, 'Reset keeps entered values');
assert.equal(ctx.simInitialInput({ name: 'sensor', type: 'bool', schema: scalar('bool'), default_text: 'false' }), 'false', 'changed input type must discard an incompatible old value');
ctx.entryNode = () => ({ name: 'BeforeStart', inputs: [{ name: 'struct_before_start', type: { kind: 'Named', name: 'Sensor' } }, { name: 'scalar_before_start', type: { kind: 'Bool' } }], locals: [], outputs: [] });
ctx.buildWatchTable();
const preRows = document.getElementById('sim-watch-body').children;
assert.match(preRows[0].children[1].textContent, /Start simulation to edit typed Sensor/);
assert.equal(state.sim.values.struct_before_start, undefined, 'pre-session watch must not fabricate scalar zero for a struct');
assert.equal(preRows[2].children[0].children[0].textContent, 'Start typed input editor');
state.sim.values.scalar_before_start = 'true';
assert.equal(ctx.simInitialInput({ name: 'scalar_before_start', type: 'bool', schema: scalar('bool'), default_text: 'false' }), 'true', 'scalar values entered before starting remain sticky');
assert.match(html, /\.watch-type code \{[^}]*overflow-wrap:anywhere/);
assert.match(html, /\.watch-structured \{[^}]*resize:both/);
assert.match(html, /<h4>New struct<\/h4>/);
assert.match(html, /kind: "record"/, 'serialized create payload remains compatible');
console.log('PASS struct input UI: generated typed defaults, fields/aliases/nested arrays, precise shape errors, lossless numeric text, sticky reset, type changes, enum selection, accessible multiline controls and error recovery. Deterministic DOM tests only.');
