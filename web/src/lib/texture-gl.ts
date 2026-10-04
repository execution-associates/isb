// The Execution Associates texture, drawn by WebGL: a fixed canvas behind the
// page with a still film grain printed onto the page (it scrolls with the
// content on whole pixels, so it reads as paper and never shimmers) and the
// evening's light spilling into the ink as an ordered dither in 2px cells.
// Ported from executionassociates.com's own texture.js (same grain, same
// Bayer threshold, same quantised light), with its scene lights replaced by
// two app-sized ones: a glow at the top of the page (peach falling to violet)
// and a faint one at the foot of the sidebar.
//
// It is cheap by construction: it draws once, then again only when the page
// scrolls or resizes, never on a timer, and not at all while the tab is hidden.
import { levelParams, type Texture } from "@/lib/texture";

const VS = "attribute vec2 p;void main(){gl_Position=vec4(p,0.,1.);}";

const FS = `
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
uniform vec2 uRes;
uniform float uScroll, uGrain, uGlow, uStep, uSide;
#define B2(a) fract(dot(floor(a),vec2(.5,floor(a).y*.75)))
float b4(vec2 a){return B2(.5*a)*.25+B2(a);}
float b8(vec2 a){return b4(.5*a)*.25+B2(a);}
float hash(vec2 p){p=fract(p*vec2(443.897,441.423));p+=dot(p,p.yx+19.19);return fract((p.x+p.y)*p.x);}
// the evening, from the lamp outward: peach, coral, magenta, violet
vec3 light(float t){
  vec3 a=vec3(.996,.745,.459), b=vec3(.969,.518,.471), c=vec3(.867,.318,.608), d=vec3(.467,.298,.655);
  return t<.35?mix(a,b,t/.35):t<.7?mix(b,c,(t-.35)/.35):mix(c,d,clamp((t-.7)/.3,0.,1.));
}
void main(){
  vec2 p=vec2(gl_FragCoord.x,uRes.y-gl_FragCoord.y);
  vec2 pp=p+vec2(0.,uScroll);
  // the lamp over the page's head: belongs to the page, so it scrolls away
  vec2 q=(pp-vec2(uRes.x*.78,-60.))/vec2(max(uRes.x*.6,560.),620.);
  float d=length(q);
  float s=pow(max(1.-d,0.),2.);
  vec3 col=light(clamp(d,0.,1.))*s; float amt=s;
  // a lower, cooler one at the foot of the sidebar, fixed to the window
  vec2 q2=(p-vec2(30.,uRes.y+70.))/vec2(330.,360.);
  float s2=pow(max(1.-length(q2),0.),2.)*.55*uSide;
  col+=light(.85)*s2; amt+=s2;
  vec3 lc=amt>0.?col/amt:vec3(.47,.3,.65);
  float a=amt*uGlow;
  // ordered dither in 2px cells: quantised light, the retro look of the brand
  float qa=clamp(floor(a/uStep+b8(pp/2.))*uStep,0.,uGlow);
  vec3 rgb=lc*qa; float al=qa;
  // still film grain, one speck per CSS pixel
  float g=(hash(pp)-.5)*uGrain;
  float ga=abs(g); vec3 gc=vec3(step(0.,g));
  rgb=gc*ga+rgb*(1.-ga); al=ga+al*(1.-ga);
  gl_FragColor=vec4(rgb,al);
}`;

export interface Gl {
  /** True for a software rasteriser (SwiftShader, llvmpipe): the caller should not use it. */
  software: boolean;
  /** Call when the page scrolls, resizes or becomes visible again. */
  kick(): void;
  stop(): void;
}

/** Start the layer on `canvas`; null when WebGL or the shader is unavailable. */
export function startGl(canvas: HTMLCanvasElement, texture: Texture, onLost: () => void): Gl | null {
  const gl = canvas.getContext("webgl", {
    alpha: true,
    premultipliedAlpha: true,
    antialias: false,
    depth: false,
    stencil: false,
    powerPreference: "low-power",
  });
  if (!gl) return null;
  let software = false;
  try {
    const dbg = gl.getExtension("WEBGL_debug_renderer_info");
    software = /swiftshader|llvmpipe|software/i.test(dbg ? String(gl.getParameter(dbg.UNMASKED_RENDERER_WEBGL)) : "");
  } catch {
    // unknown renderer: assume a real one
  }

  const compile = (type: number, src: string) => {
    const o = gl.createShader(type)!;
    gl.shaderSource(o, src);
    gl.compileShader(o);
    return gl.getShaderParameter(o, gl.COMPILE_STATUS) ? o : null;
  };
  const vs = compile(gl.VERTEX_SHADER, VS);
  const fs = compile(gl.FRAGMENT_SHADER, FS);
  if (!vs || !fs) return null;
  const prog = gl.createProgram();
  gl.attachShader(prog, vs);
  gl.attachShader(prog, fs);
  gl.linkProgram(prog);
  if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) return null;
  gl.useProgram(prog);
  gl.bindBuffer(gl.ARRAY_BUFFER, gl.createBuffer());
  gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 3, -1, -1, 3]), gl.STATIC_DRAW);
  const loc = gl.getAttribLocation(prog, "p");
  gl.enableVertexAttribArray(loc);
  gl.vertexAttribPointer(loc, 2, gl.FLOAT, false, 0, 0);
  const U = Object.fromEntries(["uRes", "uScroll", "uGrain", "uGlow", "uStep", "uSide"].map((n) => [n, gl.getUniformLocation(prog, n)]));
  const { grain, glow } = levelParams(texture);

  let W = 0;
  let H = 0;
  let raf = 0;
  let stopped = false;

  const draw = () => {
    raf = 0;
    if (stopped || document.hidden) return;
    gl.uniform2f(U.uRes, W, H);
    // whole pixels keep the grain locked to the page instead of shimmering between them
    gl.uniform1f(U.uScroll, Math.round(window.scrollY) % 4096);
    gl.uniform1f(U.uGrain, grain);
    gl.uniform1f(U.uGlow, glow);
    gl.uniform1f(U.uStep, Math.max(glow / 5, 0.01));
    gl.uniform1f(U.uSide, W >= 768 ? 1 : 0);
    gl.clearColor(0, 0, 0, 0);
    gl.clear(gl.COLOR_BUFFER_BIT);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
  };
  const kick = () => {
    if (!raf && !stopped) raf = requestAnimationFrame(draw);
  };
  const resize = () => {
    W = window.innerWidth;
    H = window.innerHeight;
    // resizing clears the canvas, so draw in the same task
    canvas.width = W;
    canvas.height = H;
    gl.viewport(0, 0, W, H);
    draw();
  };
  const lost = (e: Event) => {
    e.preventDefault();
    onLost();
  };
  window.addEventListener("resize", resize, { passive: true });
  window.addEventListener("scroll", kick, { passive: true });
  document.addEventListener("visibilitychange", kick);
  canvas.addEventListener("webglcontextlost", lost);
  resize();

  return {
    software,
    kick,
    stop() {
      stopped = true;
      if (raf) cancelAnimationFrame(raf);
      window.removeEventListener("resize", resize);
      window.removeEventListener("scroll", kick);
      document.removeEventListener("visibilitychange", kick);
      canvas.removeEventListener("webglcontextlost", lost);
      gl.getExtension("WEBGL_lose_context")?.loseContext();
    },
  };
}
