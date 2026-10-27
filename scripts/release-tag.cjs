const { appendFileSync } = require('node:fs');

async function tagCommit(api, tag) {
  let ref;
  try { ref = await api('GET', `git/ref/tags/${encodeURIComponent(tag)}`); }
  catch (error) { if (error.status === 404) return null; throw error; }
  let object = ref.object;
  for (let depth = 0; object.type === 'tag' && depth < 8; depth++) {
    object = (await api('GET', `git/tags/${object.sha}`)).object;
  }
  if (object.type !== 'commit') throw new Error(`Release tag ${tag} does not resolve to a commit`);
  return object.sha;
}

async function verify(api, tag, sha) {
  const actual = await tagCommit(api, tag);
  if (actual !== sha) throw new Error(`Release tag ${tag} must point to ${sha}; found ${actual ?? 'no tag'}`);
}

async function prepare(api, { tag, sha, branch }) {
  const existing = await tagCommit(api, tag);
  if (existing !== null) {
    if (existing !== sha) throw new Error(`Refusing to replace release tag ${tag}: ${existing} differs from ${sha}`);
    return true;
  }
  const current = async () => (await api('GET', `git/ref/heads/${encodeURIComponent(branch)}`)).object.sha;
  if (await current() !== sha) return false;
  try { await api('POST', 'git/refs', { ref: `refs/tags/${tag}`, sha }); }
  catch (error) {
    if (error.status === 422) {
      await verify(api, tag, sha);
      return true;
    }
    if (error.status === 403 && await current() !== sha) return false;
    throw error;
  }
  await verify(api, tag, sha);
  return true;
}

async function main(env, mode) {
  if (!['prepare', 'verify'].includes(mode)) throw new Error('Expected prepare or verify');
  const { GITHUB_REPOSITORY: repo, GITHUB_SHA: sha, GITHUB_REF: ref, GITHUB_RUN_NUMBER: run, GH_TOKEN: token } = env;
  if (!/^[\w.-]+\/[\w.-]+$/.test(repo ?? '') || !/^[a-f0-9]{40}$/.test(sha ?? '')
      || !/^refs\/heads\/(main|master)$/.test(ref ?? '') || !/^\d+$/.test(run ?? '')
      || env.GITHUB_EVENT_NAME !== 'push' || !token) {
    throw new Error('Release tags require an authenticated main/master push context');
  }
  if (mode === 'prepare' && !env.GITHUB_OUTPUT) throw new Error('GITHUB_OUTPUT is required');
  const tag = `v0.1.0-pre.${run}`;
  const api = async (method, path, body) => {
    const response = await fetch(`${env.GITHUB_API_URL || 'https://api.github.com'}/repos/${repo}/${path}`, {
      method,
      headers: { Authorization: `Bearer ${token}`, Accept: 'application/vnd.github+json',
        'X-GitHub-Api-Version': '2022-11-28', 'Content-Type': 'application/json' },
      body: body === undefined ? undefined : JSON.stringify(body),
      signal: AbortSignal.timeout(30_000),
    });
    if (!response.ok) {
      const error = new Error(`Release tag API returned HTTP ${response.status}`);
      error.status = response.status;
      throw error;
    }
    return response.json();
  };
  if (mode === 'verify') {
    await verify(api, tag, sha);
    console.log(`Verified ${tag} at ${sha}`);
    return;
  }
  const ready = await prepare(api, { tag, sha, branch: ref.slice('refs/heads/'.length) });
  appendFileSync(env.GITHUB_OUTPUT, `tag=${tag}\nready=${ready}\n`);
  console.log(ready ? `Reserved ${tag} at ${sha}` : 'Publication skipped: a newer push superseded this run before tag preparation');
}

module.exports = { prepare, verify, main };
if (require.main === module) main(process.env, process.argv[2]).catch(error => {
  console.error(error.message);
  process.exitCode = 1;
});
