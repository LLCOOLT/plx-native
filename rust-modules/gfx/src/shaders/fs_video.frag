// The desktop's decoded picture (`gfx::video`, desktop builds only): Y'CbCr planes as the decoder
// left them, converted to the UI's R'G'B' here, and composited UNDER the finished UI frame.
//
// One full-canvas quad: v_uv runs over the whole 1920x1080 canvas, and `u_vid` is the picture's
// own rectangle inside it (offset.xy, size.zw, canvas-normalized). Outside it the bars are black,
// which is what a television's video plane shows around a picture of another shape.
//
// PLANES. `u_y` always holds luma; chroma is either two planes (`u_u`, `u_v`) or one interleaved
// plane in `u_u` (`u_nv` = 1: NV12, P010). A sample is either one byte per channel (`u_wide` = 0)
// or a 16-bit little-endian word stored as two bytes, low then high, rebuilt here (`u_wide` = 1):
// GLES2 has no 16-bit texture formats, so the two-byte form is the one every driver accepts, and
// linear filtering commutes with `lo + 256 * hi`, so a filtered texel is still the filtered word.
// The second byte of a two-channel texture is `.g` on a core profile (RG8) and `.a` on GLES2
// (LUMINANCE_ALPHA): `u_hi_a` picks it.
//
// LEVELS. `u_lev` = (luma offset, luma gain, chroma offset, chroma gain) on the normalized code,
// worked out on the CPU for the stream's range and bit depth (`gfx::video::levels`).
//
// COLOUR. `u_k` = (Kr, Kb) of the stream's matrix (BT.601, BT.709 or BT.2020 non-constant). An SDR
// picture's R'G'B' is shown as is. `u_hdr` = 1 (PQ) or 2 (HLG) linearizes, tone-maps into SDR,
// converts BT.2020 primaries to BT.709 and re-encodes — an approximation of what an HDR display
// pipeline does, not a mastering-grade one, so that HDR10 is watchable on an SDR desktop instead of
// grey and washed out.
precision highp float;
varying highp vec2 v_uv;
uniform sampler2D u_y;
uniform sampler2D u_u;
uniform sampler2D u_v;
uniform highp vec4 u_vid;
uniform float u_nv;
uniform float u_wide;
uniform float u_hi_a;
uniform highp float u_max;
uniform highp vec4 u_lev;
uniform vec2 u_k;
uniform float u_hdr;

highp float word(highp float lo, highp float hi) {
  return (lo * 255.0 + hi * 65280.0) / u_max;
}
highp float one(sampler2D t, highp vec2 uv) {
  highp vec4 s = texture2D(t, uv);
  return u_wide > 0.5 ? word(s.r, mix(s.g, s.a, u_hi_a)) : s.r;
}
highp vec2 two(sampler2D t, highp vec2 uv) {
  highp vec4 s = texture2D(t, uv);
  if (u_wide > 0.5) return vec2(word(s.r, s.g), word(s.b, s.a));
  return vec2(s.r, mix(s.g, s.a, u_hi_a));
}

highp vec3 pq_eotf(highp vec3 e) {
  // SMPTE ST 2084, to linear light where 1.0 = 10000 cd/m2.
  highp vec3 p = pow(max(e, 0.0), vec3(1.0 / 78.84375));
  return pow(max(p - 0.8359375, 0.0) / (18.8515625 - 18.6875 * p), vec3(1.0 / 0.1593017578125));
}
highp vec3 hlg_inverse_oetf(highp vec3 e) {
  // ARIB STD-B67, to scene light in [0, 1].
  highp vec3 lo = e * e / 3.0;
  highp vec3 hi = (exp((e - 0.55991073) / 0.17883277) + 0.28466892) / 12.0;
  return mix(lo, hi, step(0.5, e));
}

void main() {
  highp vec2 at = (v_uv - u_vid.xy) / u_vid.zw;
  if (at.x < 0.0 || at.y < 0.0 || at.x > 1.0 || at.y > 1.0) {
    gl_FragColor = vec4(0.0, 0.0, 0.0, 1.0);
    return;
  }
  highp float y = one(u_y, at);
  highp vec2 c = u_nv > 0.5 ? two(u_u, at) : vec2(one(u_u, at), one(u_v, at));
  y = (y - u_lev.x) * u_lev.y;
  c = (c - u_lev.z) * u_lev.w;
  highp float kr = u_k.x;
  highp float kb = u_k.y;
  highp float kg = 1.0 - kr - kb;
  highp vec3 rgb;
  rgb.r = y + 2.0 * (1.0 - kr) * c.y;
  rgb.b = y + 2.0 * (1.0 - kb) * c.x;
  rgb.g = (y - kr * rgb.r - kb * rgb.b) / kg;
  rgb = clamp(rgb, 0.0, 1.0);
  if (u_hdr > 0.5) {
    // Linear light in units of SDR reference white (203 cd/m2, BT.2408).
    highp vec3 lin;
    if (u_hdr < 1.5) {
      lin = pq_eotf(rgb) * (10000.0 / 203.0);
    } else {
      // HLG on a 1000 cd/m2 reference display: the OOTF's system gamma is 1.2.
      highp vec3 s = hlg_inverse_oetf(rgb);
      highp float ys = dot(s, vec3(0.2627, 0.6780, 0.0593));
      lin = s * pow(max(ys, 1e-6), 0.2) * (1000.0 / 203.0);
    }
    // BT.2020 primaries to BT.709 (BT.2087), then an extended-Reinhard curve on luminance that
    // keeps reference white where SDR puts it and rolls the highlights off toward 4x it.
    lin = max(mat3(1.6605, -0.1246, -0.0182,
                   -0.5876, 1.1329, -0.1006,
                   -0.0728, -0.0083, 1.1187) * lin, 0.0);
    highp float l = dot(lin, vec3(0.2126, 0.7152, 0.0722));
    highp float w = 4.0;
    highp float m = l * (1.0 + l / (w * w)) / (1.0 + l);
    lin *= l > 0.0 ? m / l : 0.0;
    rgb = pow(clamp(lin, 0.0, 1.0), vec3(1.0 / 2.2));
  }
  gl_FragColor = vec4(rgb, 1.0);
}
