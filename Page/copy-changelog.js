const fs = require('fs');
const path = require('path');

const rootDir = path.resolve(__dirname, '..');
const pageDir = __dirname;

const copy = (src, dest) => {
  if (fs.existsSync(src)) {
    fs.copyFileSync(src, dest);
    console.log(`Copied ${src} -> ${dest}`);
  } else {
    console.warn(`Warning: Source file ${src} does not exist.`);
  }
};

copy(path.join(rootDir, 'Changelog.md'), path.join(pageDir, 'changelog.md'));
copy(path.join(rootDir, 'Changelog-zh.md'), path.join(pageDir, 'zh', 'changelog.md'));
