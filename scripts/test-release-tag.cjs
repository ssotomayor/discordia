const assert = require('node:assert/strict');
const { test } = require('node:test');
const { prepare, verify, main } = require('./release-tag.cjs');

const sha = 'a'.repeat(40);
const newer = 'b'.repeat(40);
const tag = 'v0.1.0-pre.943';
const options = { tag, sha, branch: 'master' };
const error = status => Object.assign(new Error(`HTTP ${status}`), { status });

function fixture({ head = sha, tagged = null, createError, raceHead, raceTag } = {}) {
  const calls = [];
  const api = async (method, path, body) => {
    calls.push({ method, path, body });
    if (method === 'GET' && path === `git/ref/tags/${tag}`) {
      if (!tagged) throw error(404);
      return { object: { type: 'commit', sha: tagged } };
    }
    if (method === 'GET' && path === 'git/ref/heads/master') return { object: { sha: head } };
    if (method === 'POST' && path === 'git/refs') {
      if (raceHead) head = raceHead;
      if (raceTag) tagged = raceTag;
      if (createError) throw error(createError);
      tagged = body.sha;
      return { object: { type: 'commit', sha: tagged } };
    }
    throw new Error(`Unexpected operation: ${method} ${path}`);
  };
  return { api, calls };
}

test('reserves the exact build commit and verifies the created tag', async () => {
  const { api, calls } = fixture();
  assert.equal(await prepare(api, options), true);
  assert.deepEqual(calls.filter(c => c.method === 'POST'), [
    { method: 'POST', path: 'git/refs', body: { ref: `refs/tags/${tag}`, sha } },
  ]);
  assert.equal(calls.at(-1).path, `git/ref/tags/${tag}`);
});

test('a queued obsolete run does not tag a different commit or publish', async () => {
  const { api, calls } = fixture({ head: newer });
  assert.equal(await prepare(api, options), false);
  assert.ok(calls.every(c => c.method === 'GET'));
});

test('reruns reuse the matching tag even after master advances', async () => {
  const { api, calls } = fixture({ head: newer, tagged: sha });
  assert.equal(await prepare(api, options), true);
  assert.ok(calls.every(c => c.method === 'GET'));
});

test('existing tags for another commit are rejected without rewriting them', async () => {
  const { api, calls } = fixture({ tagged: newer });
  await assert.rejects(prepare(api, options), /Refusing to replace/);
  assert.ok(calls.every(c => c.method === 'GET'));
});

test('a tag creation race is accepted only when it reserved the same commit', async () => {
  assert.equal(await prepare(fixture({ createError: 422, raceTag: sha }).api, options), true);
  await assert.rejects(prepare(fixture({ createError: 422, raceTag: newer }).api, options), /must point/);
});

test('a master advance during creation is skipped, while real permission failures remain errors', async () => {
  assert.equal(await prepare(fixture({ createError: 403, raceHead: newer }).api, options), false);
  await assert.rejects(prepare(fixture({ createError: 403 }).api, options), { status: 403 });
  await assert.rejects(prepare(fixture({ createError: 500 }).api, options), { status: 500 });
});

test('publishing rejects missing or moved tags and never repairs them', async () => {
  await assert.rejects(verify(fixture().api, tag, sha), /no tag/);
  await assert.rejects(verify(fixture({ tagged: newer }).api, tag, sha), /must point/);
  await verify(fixture({ tagged: sha }).api, tag, sha);
});

test('annotated tags are checked against their peeled commit', async () => {
  const api = async (_, path) => path.startsWith('git/tags/')
    ? { object: { type: 'commit', sha } }
    : { object: { type: 'tag', sha: newer } };
  await verify(api, tag, sha);
});

test('PR and malformed contexts cannot write tags', async () => {
  const context = { GITHUB_REPOSITORY: 'owner/repo', GITHUB_SHA: sha,
    GITHUB_REF: 'refs/heads/master', GITHUB_RUN_NUMBER: '943', GH_TOKEN: 'not-a-real-token',
    GITHUB_EVENT_NAME: 'push' };
  for (const ref of ['refs/pull/402/merge', 'refs/heads/feature']) {
    await assert.rejects(main({ ...context, GITHUB_REF: ref }, 'prepare'), /push context/);
  }
  await assert.rejects(main({ ...context, GITHUB_EVENT_NAME: 'pull_request_target' }, 'prepare'), /push context/);
  await assert.rejects(main(context, 'prepare'), /GITHUB_OUTPUT is required/);
});
