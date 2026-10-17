// Draws Rovik's icon and writes every file the app and the site take from it:
//
//   src-tauri/icons/   32x32.png, 128x128.png, 128x128@2x.png, icon.png,
//                      icon.ico (Windows), icon.icns (macOS)
//   website/public/    favicon.svg, favicon.ico, apple-touch-icon.png
//   website/src/assets icon.svg (the logo the page shows)
//
//   npm run icons      (from website/)
//
// The icon is an "R" whose bowl is a prompt's chevron, its leg in the
// accent blue, on a dark tile. The drawing is below; each platform gets the
// tile at the size its icons use: macOS on Apple's grid (an 824 px tile on a
// 1024 px canvas, with a shadow), Windows and Linux larger and flat, a
// browser tab edge to edge.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { Resvg } from '@resvg/resvg-js';

const website = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const appIcons = path.resolve(website, '../src-tauri/icons');

// The tile: a superellipse (Apple's continuous corners) through 48 points,
// joined by a Catmull-Rom spline.
function tilePath(half) {
  const n = 5;
  const points = [];
  for (let i = 0; i < 48; i++) {
    const t = (i / 48) * Math.PI * 2;
    const c = Math.cos(t);
    const s = Math.sin(t);
    points.push([
      512 + half * Math.sign(c) * Math.abs(c) ** (2 / n),
      512 + half * Math.sign(s) * Math.abs(s) ** (2 / n),
    ]);
  }
  const f = (v) => +v.toFixed(1);
  let d = `M${f(points[0][0])} ${f(points[0][1])}`;
  for (let i = 0; i < points.length; i++) {
    const [p0, p1, p2, p3] = [-1, 0, 1, 2].map((k) => points[(i + k + 48) % 48]);
    const c1 = [p1[0] + (p2[0] - p0[0]) / 6, p1[1] + (p2[1] - p0[1]) / 6];
    const c2 = [p2[0] - (p3[0] - p1[0]) / 6, p2[1] - (p3[1] - p1[1]) / 6];
    d += `C${f(c1[0])} ${f(c1[1])} ${f(c2[0])} ${f(c2[1])} ${f(p2[0])} ${f(p2[1])}`;
  }
  return `${d}Z`;
}

// The R on a 1024 grid, drawn for an 824 px tile; `scale` fits it to others.
const LEG = 'M536 512L652 724';
const STEM_AND_BOWL = 'M384 724V300H536L640 406L536 512H384';

/**
 * @param {object} o
 * @param {number} o.half    half the tile's side (512 fills the canvas)
 * @param {boolean} [o.square] a square tile (iOS rounds it itself)
 * @param {boolean} [o.shadow] macOS's drop shadow under the tile
 */
function icon({ half, square = false, shadow = false }) {
  const scale = 1.08 * (half / 412);
  const tile = square ? 'M0 0H1024V1024H0Z' : tilePath(half);
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024">
  <defs>
    <linearGradient id="tile" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#1a2440"/>
      <stop offset="1" stop-color="#0a0e1a"/>
    </linearGradient>
    <radialGradient id="sheen" cx="0.5" cy="0" r="0.9">
      <stop offset="0" stop-color="#fff" stop-opacity="0.1"/>
      <stop offset="1" stop-color="#fff" stop-opacity="0"/>
    </radialGradient>
    <linearGradient id="leg" gradientUnits="userSpaceOnUse" x1="540" y1="512" x2="660" y2="724">
      <stop offset="0" stop-color="#6f9bff"/>
      <stop offset="1" stop-color="#2f5ff0"/>
    </linearGradient>${
      shadow
        ? `
    <filter id="blur" x="-10%" y="-10%" width="120%" height="120%" color-interpolation-filters="sRGB">
      <feGaussianBlur stdDeviation="14"/>
    </filter>`
        : ''
    }
    <path id="t" d="${tile}"/>
  </defs>${
    // A blurred copy under the tile: a filter on the tile itself would
    // pass its dark gradient through 8-bit linear RGB, which bands it.
    shadow
      ? `
  <use href="#t" transform="translate(0 12)" fill="#000" fill-opacity="0.35" filter="url(#blur)"/>`
      : ''
  }
  <use href="#t" fill="url(#tile)"/>
  <use href="#t" fill="url(#sheen)"/>${
    square
      ? ''
      : `
  <use href="#t" fill="none" stroke="#fff" stroke-opacity="0.1" stroke-width="4"/>`
  }
  <g transform="translate(512 512) scale(${+scale.toFixed(4)}) translate(-518 -512)" fill="none" stroke-width="96" stroke-linecap="round" stroke-linejoin="round">
    <path d="${LEG}" stroke="url(#leg)"/>
    <path d="${STEM_AND_BOWL}" stroke="#eef2fb"/>
  </g>
</svg>
`;
}

const variants = {
  macos: icon({ half: 412, shadow: true }),
  app: icon({ half: 448 }),
  web: icon({ half: 512 }),
  touch: icon({ half: 512, square: true }),
};

function png(svg, size) {
  return new Resvg(svg, { fitTo: { mode: 'width', value: size } }).render().asPng();
}

// .ico: PNG entries, as the icon Tauri made had.
function ico(svg, sizes) {
  const images = sizes.map((size) => png(svg, size));
  const header = Buffer.alloc(6 + 16 * sizes.length);
  header.writeUInt16LE(0, 0);
  header.writeUInt16LE(1, 2);
  header.writeUInt16LE(sizes.length, 4);
  let offset = header.length;
  sizes.forEach((size, i) => {
    const entry = 6 + 16 * i;
    header.writeUInt8(size >= 256 ? 0 : size, entry);
    header.writeUInt8(size >= 256 ? 0 : size, entry + 1);
    header.writeUInt16LE(1, entry + 4); // planes
    header.writeUInt16LE(32, entry + 6); // bits per pixel
    header.writeUInt32LE(images[i].length, entry + 8);
    header.writeUInt32LE(offset, entry + 12);
    offset += images[i].length;
  });
  return Buffer.concat([header, ...images]);
}

// .icns: the PNG types macOS reads since 10.8, with a table of contents.
function icns(svg) {
  const types = [
    ['ic07', 128],
    ['ic08', 256],
    ['ic09', 512],
    ['ic10', 1024],
    ['ic11', 32],
    ['ic12', 64],
    ['ic13', 256],
    ['ic14', 512],
  ];
  const chunk = (type, data) => {
    const head = Buffer.alloc(8);
    head.write(type, 0, 'latin1');
    head.writeUInt32BE(8 + data.length, 4);
    return Buffer.concat([head, data]);
  };
  const entries = types.map(([type, size]) => chunk(type, png(svg, size)));
  const toc = Buffer.concat(entries.map((e) => e.subarray(0, 8)));
  const body = Buffer.concat([chunk('TOC ', toc), ...entries]);
  const head = Buffer.alloc(8);
  head.write('icns', 0, 'latin1');
  head.writeUInt32BE(8 + body.length, 4);
  return Buffer.concat([head, body]);
}

function write(file, data) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, data);
  console.log(`${path.relative(path.resolve(website, '..'), file)} (${data.length} bytes)`);
}

write(path.join(appIcons, '32x32.png'), png(variants.app, 32));
write(path.join(appIcons, '128x128.png'), png(variants.app, 128));
write(path.join(appIcons, '128x128@2x.png'), png(variants.app, 256));
write(path.join(appIcons, 'icon.png'), png(variants.app, 512));
write(path.join(appIcons, 'icon.ico'), ico(variants.app, [32, 16, 24, 48, 64, 256]));
write(path.join(appIcons, 'icon.icns'), icns(variants.macos));

write(path.join(website, 'public/favicon.svg'), variants.web);
write(path.join(website, 'public/favicon.ico'), ico(variants.web, [16, 32, 48]));
write(path.join(website, 'public/apple-touch-icon.png'), png(variants.touch, 180));
write(path.join(website, 'src/assets/icon.svg'), variants.web);
