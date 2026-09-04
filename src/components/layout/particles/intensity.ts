/** 运行激烈度：随运行时间从平静绿逐渐变激烈（spec 修订：颜色跟时间走） */
export interface RunIntensity {
  saturate: number;
  brightness: number;
  /** deg，向黄绿偏移 */
  hueRotate: number;
}

export const INTENSITY_FULL_MS = 300_000; // 5 分钟到满强度

/** 满强度端点：0ms → (1,1,0)；INTENSITY_FULL_MS → (1.45, 1.25, 25) */
const FULL: RunIntensity = { saturate: 1.45, brightness: 1.25, hueRotate: 25 };

/** 线性插值，超时钳制，负值按 0 */
export function runIntensity(elapsedMs: number): RunIntensity {
  const t = Math.min(Math.max(elapsedMs, 0) / INTENSITY_FULL_MS, 1);
  return {
    saturate: 1 + (FULL.saturate - 1) * t,
    brightness: 1 + (FULL.brightness - 1) * t,
    hueRotate: FULL.hueRotate * t,
  };
}
