// Gates use held-out retrieval and paired task outcomes, not vendor scores.
export function memoryGate(report, tasks) {
  const reasons = [];
  const heldout = report.groups?.heldout;
  if (
    !heldout ||
    !Number.isFinite(heldout.recall_at_8) ||
    heldout.recall_at_8 < 0.9 ||
    heldout.recall_at_8 > 1
  )
    reasons.push('Held-out Recall@8 is below 90%.');
  if (
    !heldout ||
    !Number.isFinite(heldout.no_match_injection_rate) ||
    heldout.no_match_injection_rate > 0.05 ||
    heldout.no_match_injection_rate < 0
  )
    reasons.push('No-match injection exceeds 5%.');
  if (
    !Array.isArray(report.cases) ||
    report.cases.length === 0 ||
    report.cases.some((row) => row.prohibited !== false)
  )
    reasons.push('Scope isolation is unverified or violated.');
  if (!Array.isArray(tasks) || tasks.length < 30) {
    reasons.push('At least 30 paired held-out agent tasks are required.');
  } else {
    const ids = new Set();
    const models = new Set();
    const configurations = new Set();
    let irrelevantTasks = 0;
    let off = 0;
    let on = 0;
    let irrelevantRegression = false;
    for (const task of tasks) {
      if (
        !task.id ||
        ids.has(task.id) ||
        task.split !== 'heldout' ||
        typeof task.memoryRelevant !== 'boolean' ||
        typeof task.memoryOff?.passed !== 'boolean' ||
        typeof task.memoryOn?.passed !== 'boolean' ||
        !task.memoryOff.model ||
        task.memoryOff.model !== task.memoryOn.model ||
        !task.memoryOff.configuration ||
        task.memoryOff.configuration !== task.memoryOn.configuration
      ) {
        reasons.push(
          'Task pairs must be classified, unique, held out, and use the same model and configuration.'
        );
        break;
      }
      ids.add(task.id);
      models.add(task.memoryOff.model);
      configurations.add(task.memoryOff.configuration);
      irrelevantTasks += Number(task.memoryRelevant === false);
      off += Number(task.memoryOff.passed);
      on += Number(task.memoryOn.passed);
      irrelevantRegression ||=
        task.memoryRelevant === false && task.memoryOff.passed && !task.memoryOn.passed;
    }
    if (models.size !== 1) reasons.push('All task pairs must use the same pinned model version.');
    if (configurations.size !== 1)
      reasons.push(
        'All task pairs must use the same pinned tools, budget and check configuration.'
      );
    if (irrelevantTasks === 0) reasons.push('Memory-irrelevant control tasks are required.');
    if ((on - off) / tasks.length < 0.1)
      reasons.push('Paired task success improves by less than 10 percentage points.');
    if (irrelevantRegression) reasons.push('A memory-irrelevant task regressed.');
  }
  return { passed: reasons.length === 0, reasons };
}
