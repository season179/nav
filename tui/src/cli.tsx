#!/usr/bin/env bun
import React from 'react';
import {render} from 'ink';
import {App} from './app/App.js';

const backendPath =
	(process.argv.includes('--backend')
		? process.argv[process.argv.indexOf('--backend') + 1]
		: process.env.NAV_BACKEND) ?? '';

render(<App backendPath={backendPath} />);
