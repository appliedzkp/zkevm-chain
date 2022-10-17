#!/usr/bin/env node

import fs from 'fs';

const L1_CONTRACTS = {
  '936a70c0b28532aa22240dce21f89a8399d6ac60': 'ZkEvmL1Bridge.bin-runtime',
  '936a70c0b28532aa22240dce21f89a8399d6ac61': 'L1OptimismBridge.bin-runtime',
};
const baseAddress = BigInt('0x1111111111111111111111111111111111111111');
const path = './build/plonk-verifier';
for (const file of fs.readdirSync(path)) {
  const json = JSON.parse(fs.readFileSync(`${path}/${file}`));
  let addr = BigInt(json.config.block_gas_limit + json.instance.length).toString(16).padStart(40, '0');
  console.log({file, addr});
  L1_CONTRACTS[addr] = { code: json.runtime_code };
}

const L2_CONTRACTS = {
  '0000000000000000000000000000000000010000': 'ZkEvmL2MessageDeliverer.bin-runtime',
  '0000000000000000000000000000000000020000': 'ZkEvmL2MessageDispatcher.bin-runtime',
  '4200000000000000000000000000000000000007': 'L2OptimisimBridge.bin-runtime',
};
const L1_TEMPLATE_PATH = 'docker/geth/templates/l1-testnet.json';
const L2_TEMPLATE_PATH = 'docker/geth/templates/l2-testnet.json';

const OBJS = [
  [L1_TEMPLATE_PATH, L1_CONTRACTS],
  [L2_TEMPLATE_PATH, L2_CONTRACTS],
];

for (const [path, contracts] of OBJS) {
  const genesis = JSON.parse(fs.readFileSync(path));

  for (const addr in contracts) {
    const value = contracts[addr];
    let code;
    if (typeof value === 'string') {
      code = '0x' + fs.readFileSync('./build/contracts/' + value).toString();
    } else {
      code = value.code;
    }
    const account = genesis.alloc[addr] || { balance: '0' };
    account.code = code;
    genesis.alloc[addr] = account;
  }

  fs.writeFileSync(path, JSON.stringify(genesis, null, 2));
}
