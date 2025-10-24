#!/node
const fs = require('fs')

const [, , inputSoFullPath, outputJsonFullPath] = process.argv;
if (!inputSoFullPath || !outputJsonFullPath) {
  console.error(
    "Usage: miniv2-json-from-so.js <inpuSoFullPath> <outputJsonFullPath>",
  );
  process.exit(1);
}

const binaryData = fs.readFileSync(inputSoFullPath, "hex");
const buf = Buffer.from(binaryData, "hex");
const base64Data = buf.toString("base64").trim();
const account = {
  pubkey: "MiniV21111111111111111111111111111111111111",
  account: {
    lamports: 1551155440,
    data: [base64Data, "base64"],
    owner: "BPFLoader2111111111111111111111111111111111",
    executable: true,
    rentEpoch: 144073709551615,
    space: 79061,
  },
};

fs.writeFileSync(outputJsonFullPath, JSON.stringify(account, null, 2));
