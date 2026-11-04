import * as NodeAssert from "node:assert/strict";
import * as NodeModule from "node:module";
import * as NodePath from "node:path";
import * as NodeURL from "node:url";
import * as NodeTest from "node:test";
import { ROOT } from "./runtime.ts";
import { providerFieldsFixture } from "./providerFields.ts";
import { providerClients } from "../../../packages/client-runtime/src/providerClients.ts";
import { makeProviderSettingsSchema } from "../../../packages/contracts/src/settings.ts";

const require = NodeModule.createRequire(
  NodePath.resolve(ROOT, "packages/client-runtime/package.json"),
);
const Schema = await import(NodeURL.pathToFileURL(require.resolve("effect/Schema")).href);

NodeTest.test("provider fixtures detect validation drift without display-metadata changes", () => {
  const before = providerFieldsFixture();
  const changed = providerClients.definitions.map((driver) => {
    if (driver.driverKind !== "claudeAgent") return driver;
    const field = driver.settingsSchema.fields.autoCompactWindow;
    const replacement = Schema.String.check(
      Schema.isPattern(/^(?:|[1-9]\d{5}|1000000)$/),
      Schema.makeFilter((value) => value !== "300000"),
    ).pipe(Schema.annotateKey(Schema.resolveAnnotationsKey(field)));
    return {
      ...driver,
      settingsSchema: makeProviderSettingsSchema(
        { ...driver.settingsSchema.fields, autoCompactWindow: replacement },
        {
          order: Schema.resolveAnnotations(driver.settingsSchema).providerSettingsFormSchema.order,
        },
      ),
    };
  });
  NodeAssert.notDeepEqual(providerFieldsFixture(changed), before);
});

NodeTest.test(
  "provider fixtures detect secret-reference capability and control identity changes",
  async () => {
    const { SettingsSecretValue } =
      await import("../../../packages/contracts/src/settingsSecretValue.ts");
    const before = providerFieldsFixture();
    for (const change of ["reference", "control"]) {
      const changed = providerClients.definitions.map((driver) => {
        if (driver.driverKind !== "opencode") return driver;
        const field = driver.settingsSchema.fields.serverPassword;
        const annotation = Schema.resolveAnnotationsKey(field);
        const replacement =
          change === "reference"
            ? SettingsSecretValue.pipe(
                Schema.annotateKey({ ...annotation, settingsSecretReference: true }),
              )
            : field.pipe(
                Schema.annotateKey({
                  ...annotation,
                  providerSettingsForm: { ...annotation.providerSettingsForm, control: "text" },
                }),
              );
        return {
          ...driver,
          settingsSchema: makeProviderSettingsSchema(
            { ...driver.settingsSchema.fields, serverPassword: replacement },
            {
              order: Schema.resolveAnnotations(driver.settingsSchema).providerSettingsFormSchema
                .order,
            },
          ),
        };
      });
      NodeAssert.notDeepEqual(providerFieldsFixture(changed), before, change);
    }
  },
);
