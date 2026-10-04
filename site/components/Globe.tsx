"use client";

import { useEffect, useRef } from "react";
import * as THREE from "three";
import { LAND_B64 } from "@/lib/land";

type LatLon = readonly [number, number];

// Coarse, invented places. The real list is whatever hosts publish on the rendezvous.
const INSTANCES: LatLon[] = [
  [52.5, 13.4],
  [40.7, -74.0],
  [-34.6, -58.4],
  [35.7, 139.7],
  [1.3, 103.8],
  [-33.9, 18.4],
  [59.3, 18.1],
  [19.4, -99.1],
  [-37.8, 145.0],
  [51.5, -0.1],
];
const ME: LatLon = [45.5, -73.6];

const DOT_COUNT = 7000;
const AUTO_SPIN = 0.045;
const ARC_CYCLE = 10;
const ARC_STAGGER = 0.55;
const ARC_GROW = 1.3;
const ARC_HOLD_UNTIL = 7.2;
const ARC_FADE = 1.2;
const ARC_SEGMENTS = 72;

function toVec([lat, lon]: LatLon, r = 1): THREE.Vector3 {
  const la = THREE.MathUtils.degToRad(lat);
  const lo = THREE.MathUtils.degToRad(lon);
  return new THREE.Vector3(
    Math.cos(la) * Math.cos(lo) * r,
    Math.sin(la) * r,
    -Math.cos(la) * Math.sin(lo) * r,
  );
}

// Same walk as client/src/features/globe_geometry.rs: Fibonacci sphere filtered by the land mask.
function landDots(): Float32Array {
  const bin = atob(LAND_B64);
  const land = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) land[i] = bin.charCodeAt(i);
  const golden = Math.PI * (3 - Math.sqrt(5));
  const out: number[] = [];
  for (let i = 0; i < DOT_COUNT; i++) {
    const y = 1 - (2 * i + 1) / DOT_COUNT;
    const radial = Math.sqrt(1 - y * y);
    const angle = golden * i;
    const x = Math.cos(angle) * radial;
    const z = Math.sin(angle) * radial;
    const lat = THREE.MathUtils.radToDeg(Math.asin(y));
    const lon = THREE.MathUtils.radToDeg(Math.atan2(-z, x));
    const row = Math.min(179, Math.max(0, Math.floor(90 - lat)));
    const col = Math.min(359, Math.max(0, Math.floor(lon + 180)));
    const bit = row * 360 + col;
    if ((land[bit >> 3] >> (bit & 7)) & 1) out.push(x * 1.003, y * 1.003, z * 1.003);
  }
  return new Float32Array(out);
}

function discTexture(): THREE.Texture {
  const c = document.createElement("canvas");
  c.width = c.height = 32;
  const g = c.getContext("2d")!;
  g.fillStyle = "#fff";
  g.beginPath();
  g.arc(16, 16, 14, 0, Math.PI * 2);
  g.fill();
  return new THREE.CanvasTexture(c);
}

function arcCurve(a: THREE.Vector3, b: THREE.Vector3): THREE.QuadraticBezierCurve3 {
  const mid = a.clone().add(b).multiplyScalar(0.5);
  mid.normalize().multiplyScalar(1 + 0.12 + a.angleTo(b) * 0.22);
  return new THREE.QuadraticBezierCurve3(a, mid, b);
}

const SPHERE_VERT = /* glsl */ `
  varying vec3 vN;
  varying vec3 vV;
  void main() {
    vN = normalize(normalMatrix * normal);
    vec4 mv = modelViewMatrix * vec4(position, 1.0);
    vV = normalize(-mv.xyz);
    gl_Position = projectionMatrix * mv;
  }
`;

const SPHERE_FRAG = /* glsl */ `
  uniform vec3 base;
  uniform vec3 rim;
  varying vec3 vN;
  varying vec3 vV;
  void main() {
    float f = pow(1.0 - max(dot(normalize(vN), normalize(vV)), 0.0), 2.6);
    gl_FragColor = vec4(mix(base, rim, f), 1.0);
    #include <tonemapping_fragment>
    #include <colorspace_fragment>
  }
`;

type Arc = {
  line: THREE.Line;
  material: THREE.LineBasicMaterial;
  ring: THREE.Mesh;
  ringMaterial: THREE.MeshBasicMaterial;
  offset: number;
};

export default function Globe() {
  const hostRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;

    const reduce = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    const renderer = new THREE.WebGLRenderer({ antialias: true, alpha: true, powerPreference: "low-power" });
    renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, 2));
    renderer.setClearColor(0x000000, 0);
    const canvas = renderer.domElement;
    canvas.setAttribute("aria-hidden", "true");
    host.appendChild(canvas);

    const scene = new THREE.Scene();
    const camera = new THREE.PerspectiveCamera(50, 1, 0.1, 10);
    camera.position.set(0, 0, 3);

    const globe = new THREE.Group();
    scene.add(globe);

    const sphereGeo = new THREE.SphereGeometry(1, 72, 72);
    const sphereMat = new THREE.ShaderMaterial({
      uniforms: {
        base: { value: new THREE.Color("#15120d") },
        rim: { value: new THREE.Color("#4a3418") },
      },
      vertexShader: SPHERE_VERT,
      fragmentShader: SPHERE_FRAG,
    });
    globe.add(new THREE.Mesh(sphereGeo, sphereMat));

    const dotsGeo = new THREE.BufferGeometry();
    dotsGeo.setAttribute("position", new THREE.BufferAttribute(landDots(), 3));
    const disc = discTexture();
    const dotsMat = new THREE.PointsMaterial({
      color: "#857d72",
      size: 0.0165,
      map: disc,
      transparent: true,
      alphaTest: 0.4,
      depthWrite: false,
      sizeAttenuation: true,
    });
    globe.add(new THREE.Points(dotsGeo, dotsMat));

    const pinGeo = new THREE.SphereGeometry(0.014, 12, 12);
    const pinMat = new THREE.MeshBasicMaterial({ color: "#f5ab5c" });
    const ringGeo = new THREE.RingGeometry(0.028, 0.034, 40);
    const me = toVec(ME, 1.008);
    const meMat = new THREE.MeshBasicMaterial({ color: "#b98cff" });
    const meMesh = new THREE.Mesh(new THREE.SphereGeometry(0.022, 16, 16), meMat);
    meMesh.position.copy(me);
    globe.add(meMesh);
    const meRingMat = new THREE.MeshBasicMaterial({
      color: "#b98cff",
      transparent: true,
      side: THREE.DoubleSide,
      depthWrite: false,
    });
    const meRing = new THREE.Mesh(ringGeo, meRingMat);
    meRing.position.copy(me);
    meRing.lookAt(me.clone().multiplyScalar(2));
    globe.add(meRing);

    const arcs: Arc[] = INSTANCES.map((place, i) => {
      const p = toVec(place, 1.008);
      const pin = new THREE.Mesh(pinGeo, pinMat);
      pin.position.copy(p);
      globe.add(pin);

      const ringMaterial = new THREE.MeshBasicMaterial({
        color: "#f5ab5c",
        transparent: true,
        opacity: 0,
        side: THREE.DoubleSide,
        depthWrite: false,
      });
      const ring = new THREE.Mesh(ringGeo, ringMaterial);
      ring.position.copy(p);
      ring.lookAt(p.clone().multiplyScalar(2));
      globe.add(ring);

      const curve = arcCurve(me, p);
      const geometry = new THREE.BufferGeometry().setFromPoints(curve.getPoints(ARC_SEGMENTS));
      const material = new THREE.LineBasicMaterial({
        color: "#e8933a",
        transparent: true,
        opacity: reduce ? 0.5 : 0,
        blending: THREE.AdditiveBlending,
        depthWrite: false,
      });
      const line = new THREE.Line(geometry, material);
      if (!reduce) geometry.setDrawRange(0, 0);
      globe.add(line);
      return { line, material, ring, ringMaterial, offset: i * ARC_STAGGER };
    });

    const animateArcs = (t: number) => {
      for (const arc of arcs) {
        const local = (((t - arc.offset) % ARC_CYCLE) + ARC_CYCLE) % ARC_CYCLE;
        const grow = Math.min(1, local / ARC_GROW);
        const eased = 1 - Math.pow(1 - grow, 3);
        arc.line.geometry.setDrawRange(0, Math.max(0, Math.round(eased * ARC_SEGMENTS) + 1));
        const fade = local > ARC_HOLD_UNTIL ? Math.max(0, 1 - (local - ARC_HOLD_UNTIL) / ARC_FADE) : 1;
        arc.material.opacity = 0.65 * fade * Math.min(1, local / 0.3);
        const since = local - ARC_GROW;
        if (since >= 0 && since < 2.2) {
          const q = since / 2.2;
          arc.ring.scale.setScalar(1 + q * 2.4);
          arc.ringMaterial.opacity = (1 - q) * 0.9;
        } else {
          arc.ringMaterial.opacity = 0;
        }
      }
      const q = (t % 3) / 3;
      meRing.scale.setScalar(1 + q * 2.2);
      meRingMat.opacity = (1 - q) * 0.8;
    };
    if (reduce) {
      meRing.scale.setScalar(1.6);
      meRingMat.opacity = 0.5;
    }

    let yaw = Math.atan2(-me.x, me.z) - 0.55;
    let pitch = 0.32;
    let vel = 0;
    let dragging = false;
    let lastX = 0;
    let lastY = 0;
    let lastMove = 0;
    let visible = true;
    let running = false;
    let raf = 0;
    let last = performance.now();
    let elapsed = 0;

    const fit = () => {
      const w = host.clientWidth || 1;
      const h = host.clientHeight || 1;
      renderer.setSize(w, h, false);
      const aspect = w / h;
      camera.aspect = aspect;
      const need = aspect >= 1 ? 1.42 : 1.42 / aspect;
      camera.fov = THREE.MathUtils.radToDeg(2 * Math.atan(need / camera.position.z));
      camera.updateProjectionMatrix();
    };

    const idle = () => reduce && !dragging && Math.abs(vel) < 0.002;

    const frame = (now: number) => {
      const dt = Math.min(0.05, (now - last) / 1000);
      last = now;
      elapsed += dt;
      if (!dragging) {
        yaw += (reduce ? 0 : AUTO_SPIN) * dt + vel * dt;
        vel *= Math.pow(0.02, dt);
      }
      globe.rotation.set(pitch, yaw, 0);
      if (!reduce) animateArcs(elapsed);
      renderer.render(scene, camera);
      if (running && !idle()) {
        raf = requestAnimationFrame(frame);
      } else {
        running = false;
      }
    };

    const kick = () => {
      if (running || !visible || document.hidden) return;
      running = true;
      last = performance.now();
      raf = requestAnimationFrame(frame);
    };
    const halt = () => {
      running = false;
      cancelAnimationFrame(raf);
    };

    const onDown = (e: PointerEvent) => {
      dragging = true;
      vel = 0;
      lastX = e.clientX;
      lastY = e.clientY;
      lastMove = e.timeStamp;
      canvas.setPointerCapture(e.pointerId);
      canvas.style.cursor = "grabbing";
      kick();
    };
    const onMove = (e: PointerEvent) => {
      if (!dragging) return;
      const dx = e.clientX - lastX;
      const dy = e.clientY - lastY;
      const dt = Math.max(1 / 120, (e.timeStamp - lastMove) / 1000);
      lastX = e.clientX;
      lastY = e.clientY;
      lastMove = e.timeStamp;
      yaw += dx * 0.005;
      pitch = Math.min(0.9, Math.max(-0.9, pitch + dy * 0.004));
      vel = Math.min(3, Math.max(-3, (dx * 0.005) / dt));
      kick();
    };
    const onUp = (e: PointerEvent) => {
      if (!dragging) return;
      dragging = false;
      if (e.timeStamp - lastMove > 80) vel = 0;
      canvas.releasePointerCapture(e.pointerId);
      canvas.style.cursor = "grab";
      kick();
    };
    canvas.addEventListener("pointerdown", onDown);
    canvas.addEventListener("pointermove", onMove);
    canvas.addEventListener("pointerup", onUp);
    canvas.addEventListener("pointercancel", onUp);

    const ro = new ResizeObserver(() => {
      fit();
      kick();
      if (idle()) renderer.render(scene, camera);
    });
    ro.observe(host);
    const io = new IntersectionObserver(([entry]) => {
      visible = entry.isIntersecting;
      if (visible) kick();
      else halt();
    });
    io.observe(host);
    const onVisibility = () => (document.hidden ? halt() : kick());
    document.addEventListener("visibilitychange", onVisibility);

    fit();
    kick();
    if (idle()) renderer.render(scene, camera);

    return () => {
      halt();
      ro.disconnect();
      io.disconnect();
      document.removeEventListener("visibilitychange", onVisibility);
      canvas.removeEventListener("pointerdown", onDown);
      canvas.removeEventListener("pointermove", onMove);
      canvas.removeEventListener("pointerup", onUp);
      canvas.removeEventListener("pointercancel", onUp);
      for (const arc of arcs) {
        arc.line.geometry.dispose();
        arc.material.dispose();
        arc.ringMaterial.dispose();
      }
      sphereGeo.dispose();
      sphereMat.dispose();
      dotsGeo.dispose();
      dotsMat.dispose();
      disc.dispose();
      pinGeo.dispose();
      pinMat.dispose();
      ringGeo.dispose();
      meMesh.geometry.dispose();
      meMat.dispose();
      meRingMat.dispose();
      renderer.dispose();
      canvas.remove();
    };
  }, []);

  return (
    <div
      ref={hostRef}
      className="globe"
      role="img"
      aria-label="A dotted globe. Amber points are servers; arcs run from one violet point, your identity, to each of them."
    />
  );
}
