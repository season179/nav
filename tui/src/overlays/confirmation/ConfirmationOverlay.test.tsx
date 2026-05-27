import {describe, expect, test} from 'bun:test';
import React from 'react';
import {render} from 'ink-testing-library';
import {ConfirmationOverlay} from './ConfirmationOverlay.js';
import type {ToolApprovalRequest} from './ConfirmationOverlay.js';

describe('ConfirmationOverlay snapshots', () => {
	test('renders a bash confirmation with command-focused arguments', () => {
		expect(
			render(
				<ConfirmationOverlay
					request={approvalRequest({
						toolName: 'bash',
						reason: 'bash requires approval',
						argumentsSummary: '{"cmd":"echo hi"}',
						riskClass: 'exec',
					})}
					onApprove={() => {}}
					onReject={() => {}}
				/>,
			).lastFrame(),
		).toMatchSnapshot();
	});

	test('renders a generic tool confirmation with a wrapped reason', () => {
		expect(
			render(
				<ConfirmationOverlay
					request={approvalRequest({
						toolName: 'write_file',
						reason:
							'This write touches a file outside the current task focus and needs a human decision before the run continues.',
						argumentsSummary: '{"path":"notes.md","content":"hello"}',
						riskClass: 'mutate',
					})}
					onApprove={() => {}}
					onReject={() => {}}
				/>,
			).lastFrame(),
		).toMatchSnapshot();
	});
});

function approvalRequest(
	overrides: Partial<ToolApprovalRequest> = {},
): ToolApprovalRequest {
	return {
		approvalId: 'approval-1',
		toolCallId: 'tool-call-1',
		toolName: 'read',
		reason: 'tool requires approval',
		argumentsSummary: '',
		riskClass: undefined,
		...overrides,
	};
}
