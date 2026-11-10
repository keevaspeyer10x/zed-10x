#!/usr/bin/env node

// Single Zed 10x dev release identity: {x.y.z}+dev.{run}.{40-hex-commit}.
// The workflow, unsigned bundle, and signer all call this. AppVersion::load
// replaces build metadata, so callers stamp ZED_BUILD_ID and ZED_COMMIT_SHA
// rather than exporting ZED_APP_VERSION.

import fs from "node:fs";
import process from "node:process";
import { pathToFileURL } from "node:url";

const BASE_PATTERN = /^\d+\.\d+\.\d+$/;
const RUN_PATTERN = /^\d+$/;
const COMMIT_PATTERN = /^[0-9a-f]{40}$/;
const VERSION_PATTERN = /^\d+\.\d+\.\d+\+dev\.\d+\.[0-9a-f]{40}$/;

export function zed10xReleaseVersion({ base, run, commit }) {
  if (typeof base !== "string" || !BASE_PATTERN.test(base)) {
    throw new Error("base version must be x.y.z");
  }
  if (typeof run !== "string" || !RUN_PATTERN.test(run)) {
    throw new Error("run number must be decimal digits");
  }
  if (typeof commit !== "string" || !COMMIT_PATTERN.test(commit)) {
    throw new Error("commit must be 40 lowercase hexadecimal characters");
  }
  const version = `${base}+dev.${run}.${commit}`;
  if (!VERSION_PATTERN.test(version)) {
    throw new Error("composed version is not a Zed 10x release version");
  }
  return version;
}

export function readZedCargoBase(cargoPath) {
  const source = fs.readFileSync(cargoPath, "utf8");
  const matches = [...source.matchAll(/^version = "(\d+\.\d+\.\d+)"$/gm)];
  if (matches.length !== 1) {
    throw new Error(`${cargoPath} must contain exactly one x.y.z package version`);
  }
  return matches[0][1];
}

function takeValue(args, index, flag) {
  const value = args[index + 1];
  if (value === undefined || value.startsWith("--")) {
    throw new Error(`missing value for ${flag}`);
  }
  return value;
}

function parseArguments(args) {
  let print;
  let cargoPath;
  let base;
  let run;
  let commit;
  for (let index = 0; index < args.length; index += 1) {
    const flag = args[index];
    if (flag === "--print") {
      if (print !== undefined) throw new Error("duplicate argument --print");
      print = takeValue(args, index, flag);
      index += 1;
    } else if (flag === "--cargo") {
      if (cargoPath !== undefined) throw new Error("duplicate argument --cargo");
      cargoPath = takeValue(args, index, flag);
      index += 1;
    } else if (flag === "--base") {
      if (base !== undefined) throw new Error("duplicate argument --base");
      base = takeValue(args, index, flag);
      index += 1;
    } else if (flag === "--run") {
      if (run !== undefined) throw new Error("duplicate argument --run");
      run = takeValue(args, index, flag);
      index += 1;
    } else if (flag === "--commit") {
      if (commit !== undefined) throw new Error("duplicate argument --commit");
      commit = takeValue(args, index, flag);
      index += 1;
    } else {
      throw new Error(`unknown argument ${flag}`);
    }
  }
  if (print !== "version" && print !== "base") {
    throw new Error("--print must be version or base");
  }
  if (cargoPath !== undefined) {
    const cargoBase = readZedCargoBase(cargoPath);
    if (base !== undefined && base !== cargoBase) {
      throw new Error("base version does not match Cargo.toml");
    }
    base = cargoBase;
  }
  if (base === undefined) throw new Error("missing --cargo or --base");
  if (print === "base") {
    if (run !== undefined || commit !== undefined) {
      throw new Error("--print base does not take --run or --commit");
    }
    return { print, base };
  }
  if (run === undefined || commit === undefined) {
    throw new Error("--print version requires --run and --commit");
  }
  return { print, base, run, commit };
}

function main() {
  try {
    const options = parseArguments(process.argv.slice(2));
    const output = options.print === "base" ? options.base : zed10xReleaseVersion(options);
    process.stdout.write(`${output}\n`);
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 2;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main();
}
