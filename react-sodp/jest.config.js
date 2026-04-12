/** @type {import('jest').Config} */
module.exports = {
  preset: "ts-jest",
  testEnvironment: "jsdom",
  moduleNameMapper: {
    "^(\\.{1,2}/.*)\\.js$": "$1",
  },
  // Only run tests from src/. Without this, Jest picks up stale compiled
  // tests in dist/ after `npm run build`, which fail in jsdom because the
  // compiled msgpack dependency expects a Node environment with TextEncoder.
  testPathIgnorePatterns: ["/node_modules/", "/dist/"],
};
