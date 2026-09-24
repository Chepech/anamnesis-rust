import * as esbuild from "esbuild";
import { cpSync, mkdirSync } from "fs";

const isProduction = process.argv[2] === "production";

mkdirSync("dist", { recursive: true });
cpSync("public", "dist", { recursive: true });

await esbuild.build({
  entryPoints: ["src/main.tsx"],
  bundle: true,
  platform: "browser",
  target: ["chrome110", "safari15"],
  format: "iife",
  outfile: "dist/bundle.js",
  sourcemap: isProduction ? false : "inline",
  minify: isProduction,
  define: { "process.env.NODE_ENV": JSON.stringify(isProduction ? "production" : "development") },
  logLevel: "info",
});
