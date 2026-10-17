// Lightweight browser/chrome API compatibility shim without MPL-2.0 polyfill dependencies.
export const browser = (typeof globalThis.browser !== "undefined" && globalThis.browser)
  ? globalThis.browser
  : globalThis.chrome;

export function promisify(fn, ...args) {
  return new Promise((resolve, reject) => {
    try {
      fn(...args, (result) => {
        const err = browser.runtime.lastError;
        if (err) {
          reject(new Error(err.message));
        } else {
          resolve(result);
        }
      });
    } catch (e) {
      reject(e);
    }
  });
}
