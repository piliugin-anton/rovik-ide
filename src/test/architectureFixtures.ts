import type { ArchitectureContext, ArchitectureFeedback } from '../lib/architectureFeedback';
export const architectureSource = 'pub fn reserve() {}\n';
export const architectureContext: ArchitectureContext = {
  root: '/project',
  projectId: 'project-ui',
  chatProjectId: 'project-ui',
  chatId: 'chat',
  hostId: 'build-host',
  unsavedFiles: [],
};
export function architectureEvidence(runId = 'run-1'): ArchitectureFeedback {
  return {
    runId,
    projectId: 'repository-id',
    workspace: '/host/workspace',
    currentSnapshotDigest: 'sha256:source',
    evidenceCandidateDigest: 'sha256:source',
    configurationDigest: 'sha256:config',
    oracleDigest: 'sha256:oracle',
    policyApproved: true,
    freshness: 'current',
    policies: [
      {
        checkId: 'architecture',
        configurationDigest: 'sha256:profile',
        requestDigest: 'sha256:request',
        profileId: 'backend',
        language: 'rust',
        architecturePolicyDigest: 'sha256:policy',
        architecture: { layers: ['domain', 'infrastructure'] },
        forbiddenEdges: [{ source: 'domain', target: 'infrastructure' }],
      },
    ],
    observations: [
      {
        checkId: 'architecture',
        configurationDigest: 'sha256:profile',
        resultId: 'result-1',
        receiptId: 'receipt-1',
        graphDigest: 'sha256:graph',
        status: 'fail',
        precision: 'cargo_resolved',
        findings: [
          {
            ruleId: 'forbidden-edge',
            message: 'Domain depends on infrastructure.',
            path: ['domain', 'infrastructure'],
            locations: [
              {
                path: 'src/lib.rs',
                contentDigest:
                  'sha256:d160d9d606f4a65192da9448bc79d3795c3ea8967c7aff3cf824c60d920526d4',
              },
            ],
          },
        ],
      },
    ],
    diagnostics: [],
  };
}
