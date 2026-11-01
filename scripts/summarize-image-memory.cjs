const fs = require('node:fs');
const path = require('node:path');
const root = path.resolve(__dirname, '..');
const results = JSON.parse(fs.readFileSync(path.join(root, 'docs/RAM_PHASE8_RESULTS.json')));
const statistics = values => {
  const sorted = values.filter(Number.isFinite).sort((a, b) => a - b);
  if (!sorted.length) return undefined;
  const n = sorted.length, mean = sorted.reduce((sum, value) => sum + value, 0) / n;
  return { n, median: (sorted[Math.floor((n - 1) / 2)] + sorted[Math.floor(n / 2)]) / 2,
    min: sorted[0], max: sorted[n - 1], sampleSD: n > 1 ? Math.sqrt(sorted.reduce((sum, value) => sum + (value - mean) ** 2, 0) / (n - 1)) : 0 };
};
const metrics = {
  rustPrivateMiB: sample => sample.rustPrivateMiB,
  rendererPrivateMiB: sample => sample.rendererPrivateMiB,
  gpuPrivateMiB: sample => sample.gpuPrivateMiB,
  totalPrivateMiB: sample => sample.summary.TotalPrivateCommitMb,
  workingSetMiB: sample => sample.summary.TotalResidentWorkingSetMb,
  idleCpuPercent: sample => sample.summary.TotalCpuPercent,
  loadMs: sample => sample.loadMs,
  encodedMiB: sample => sample.cache.encodedBytes / 1048576,
  cacheEntries: sample => sample.cache.entries,
  cacheBuffers: sample => sample.cache.encodedBuffers,
  imageCount: sample => sample.imageCount,
  loadingCpuRustCoreSeconds: sample => sample.loadingCpu?.rust,
  loadingCpuRendererCoreSeconds: sample => sample.loadingCpu?.renderer,
  loadingCpuGpuCoreSeconds: sample => sample.loadingCpu?.gpu,
  loadingCpuTotalCoreSeconds: sample => sample.loadingCpu?.total,
  cpuObservationMs: sample => sample.loadingCpu?.observationMs,
};
const output = { units: 'MiB = 1048576 bytes; CPU load = cumulative core seconds; idle CPU follows existing profiler normalization; GPU Private Commit is not VRAM',
  scope: results.scope, excludedAttempts: results.excludedAttempts,
  conclusionVariants: ['paint-baseline', 'paint-leases', 'paint-thumbnails'],
  exploratoryOnly: 'Unprefixed variants explicitly called img.decode(); their memory and latency do not represent the normal production JPEG path.', variants: {} };
for (const variant of [...new Set(results.samples.map(sample => sample.variant))]) {
  if (results.excludedAttempts?.some(attempt => attempt.variant === variant)) continue;
  output.variants[variant] = {};
  for (const scenario of [...new Set(results.samples.filter(sample => sample.variant === variant).map(sample => sample.scenario))]) {
    const samples = results.samples.filter(sample => sample.variant === variant && sample.scenario === scenario);
    if (samples.length !== 5 || new Set(samples.map(sample => sample.run)).size !== 5) throw new Error(`Expected five independent runs: ${variant}/${scenario}`);
    output.variants[variant][scenario] = Object.fromEntries(Object.entries(metrics).map(([name, metric]) => [name, statistics(samples.map(metric))]).filter(([, value]) => value));
  }
}
fs.writeFileSync(path.join(root, 'docs/RAM_PHASE8_STATISTICS.json'), JSON.stringify(output, null, 2) + '\n');
const format = stats => `${stats.median.toFixed(2)} [${stats.min.toFixed(2)}–${stats.max.toFixed(2)}]; SD ${stats.sampleSD.toFixed(2)}`;
const comparison = output.conclusionVariants;
console.log('# Fase 8 — comparación completa\n');
console.log('Windows/WebView2 real; fixture del componente, no cliente completo. Cinco procesos por variante. Mediana [mínimo–máximo]; desviación estándar muestral. MiB = 1048576 bytes. CPU de carga en segundos de un núcleo; GPU Private Commit no es VRAM.\n');
console.log('Base: b753f11. Conservado: 60b70f7 (lease). Miniaturas: prototipo descartado por latencia y complejidad. Sólo series paint-*, sin img.decode() explícito.\n');
console.log('| Escenario / métrica | Base | Lease conservado | Miniaturas descartadas |');
console.log('|---|---:|---:|---:|');
for (const scenario of Object.keys(output.variants[comparison[0]])) {
  for (const metric of ['totalPrivateMiB', 'rustPrivateMiB', 'rendererPrivateMiB', 'gpuPrivateMiB', 'workingSetMiB', 'idleCpuPercent', 'loadMs', 'encodedMiB', 'loadingCpuTotalCoreSeconds']) {
    const cells = comparison.map(variant => output.variants[variant]?.[scenario]?.[metric]);
    if (cells.every(Boolean)) console.log(`| ${scenario} / ${metric} | ${cells.map(format).join(' | ')} |`);
  }
}
