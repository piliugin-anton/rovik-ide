## Submitting the preparation recipe

1. Use read to inspect `.rovik-preparation/template.json` and `.rovik-preparation/example.json`. The generated `.rovik-preparation/schema.ts` is the authoritative field-type reference. The template contains this preparation's service-owned settings; the complete worked example below shows every field and nested element type. Fill a copy of the template using the saved plan, inspected code and measured facts. Preserve its runtime, model and budget settings.
2. The document has top-level `setup`, `oracleFiles` and `negativeControlFiles`. The requirements, profiles, design and scope belong inside `setup`. In the two file lists, `path` is the intended repository-relative test/source path, while `source` is the relative path to the staged file under `.rovik-preparation/oracle/` or `.rovik-preparation/control/`. Write those staged files first. Existing protected tests can be selected without copying them.
3. Use write to save the completed document at `.rovik-preparation/submission.json`. It must already exist and contain valid JSON, without Markdown fences or comments, before submission. Read it back to check the field names and staged paths. Replace the example's illustrative task, language, paths, symbols and case IDs with inspected project facts; do not invent runtime versions, test IDs or evidence.

Field types apply to every language (Rust, C/C++, JavaScript/TypeScript and Python):
- `setup.design.components` maps component names to responsibility STRINGS, for example `{"classification":"Own numeric inclusion"}`. Do not put objects or arrays in its values.
- `setup.profiles[].facts` maps fact names to STRINGS, including versions, policies and editions: `{"edition":"2021","allocation_policy":"std"}`. Use only the required names for that language; do not put objects, numbers, booleans or arrays in fact values.
- `setup.design.publicApiChanges`, `resourceContracts`, `errorContracts` and `mechanismAlternatives` are arrays of STRINGS, for example `["Preserve the public signature."]`, never objects such as `[{"description":"..."}]`. `concurrencyContract`, `mechanismPressure` and `mechanismCost` are single STRINGS.
- `setup.design.existingCode` is an array of objects with string `file`, `symbol` and `rationale` fields.
- `setup.baselineFailures` is an array of OBJECTS: `[{"profileId":"main","configurationId":"tests","caseIds":["zero"],"rationale":"Existing defect requested by the plan"}]`. Use `[]` if no original failures are expected. Do not convert these objects to strings to work around a different field's error.
- `setup.excludedBuildRoots` is an array of OBJECTS: `[{"manifest":"examples/demo/package.json","rationale":"Unrelated example, frozen outside this task"}]`. Use `[]` if nothing is excluded; never invent an exclusion just to copy the example.
- `casesByConfiguration` maps `profileId/configurationId` to arrays of case-ID STRINGS. Paths, `negativeControlCases`, `features`, `build_targets` and `flags` are arrays of STRINGS. `typescriptProject` is a string path or null.
- Configuration and worker keys use snake_case; surrounding setup keys use camelCase. Booleans such as `default_features` are JSON booleans; budgets, durations and worker limits are JSON integers. Copy service-owned values from the template. Keep `negativeControlRevision` as `""` when submitting staged negative-control files.

Errors identify the failing field, for example `setup.design.resourceContracts[0]: invalid type: map, expected a string`. Repair that exact value against the example; do not remove required design fields or guess by rewriting unrelated sections.

Call `prepare_verified_plan` with this argument object:

```json
{
  "input_path": ".rovik-preparation/submission.json"
}
```

`input_path` is the only argument and is spelled in snake_case. Its value is a string path relative to the issued private workspace, not an absolute path or a path in the user's checkout. Pass the object above as the tool arguments; do not wrap it in `name`/`arguments`, send a bare path string, or put the recipe's `setup`, `oracleFiles` or `negativeControlFiles` directly in the call. Do not add approval flags, preparation IDs, provider credentials or runtime overrides. The service supplies the preparation identity and authority.

If the tool reports a validation error or failed trial, inspect its diagnostics, correct the JSON or staged files with write, and call the same tool with the submission path again. A text reply cannot submit the recipe. Stop once the service reports `ready` with a review: the IDE handles implementation from that point.
