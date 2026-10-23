import assert from 'node:assert/strict';
import { processCounters, memoryCounters, frameCounters, frameDelta } from './measure-android.mjs';

const stat = '42 (app worker (1)) S 0 0 0 0 0 0 0 0 0 0 123 45 999 999 0';
assert.equal(processCounters(stat), 168);
assert.throws(() => processCounters('unavailable'));
assert.deepEqual(memoryCounters('TOTAL PSS: 12000 TOTAL RSS: 23000 TOTAL SWAP PSS: 0'), { pssKiB: 12000, rssKiB: 23000 });
assert.deepEqual(memoryCounters(' TOTAL 1234 0 0'), { pssKiB: 1234, rssKiB: null });
assert.deepEqual(memoryCounters('No process found'), { pssKiB: null, rssKiB: null });
assert.deepEqual(frameDelta(frameCounters('Total frames rendered: 100\nJanky frames: 10 (10%)'), frameCounters('Total frames rendered: 120\nJanky frames: 12 (10%)')), { frames: 20, janky: 2, jankyPercent: 10 });
assert.equal(frameDelta({ frames: 100, janky: 10 }, { frames: 1, janky: 0 }), null);
assert.equal(frameDelta({ frames: 100, janky: 10 }, { frames: 100, janky: 10 }), null);
assert.equal(frameDelta(frameCounters('unavailable'), { frames: 120, janky: 12 }), null);
console.log('Android measurement parsers: process names, CPU fields, memory formats, frame deltas and counter resets passed.');
