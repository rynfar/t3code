// Mirrors ProviderSettingsForm. Keep native's form metadata tied to the real driver schemas.
import * as NodeModule from "node:module";
import * as NodePath from "node:path";
import * as NodeURL from "node:url";
import { ROOT } from "./runtime.ts";

const require = NodeModule.createRequire(
  NodePath.join(ROOT, "packages/client-runtime/package.json"),
);
const Schema = await import(NodeURL.pathToFileURL(require.resolve("effect/Schema")).href);
const { acceptsSecretReference } =
  await import("../../../packages/contracts/src/settingsSecretValue.ts");
const { isSecretSetting } = await import("../../../packages/contracts/src/settings.ts");
const { providerClients } = await import("../../../packages/client-runtime/src/providerClients.ts");

export function providerFieldsFixture(definitions = providerClients.definitions) {
  return Object.fromEntries(
    definitions.map((driver) => {
      const order =
        Schema.resolveAnnotations(driver.settingsSchema)?.providerSettingsFormSchema?.order ?? [];
      const fields = Object.entries(driver.settingsSchema.fields).flatMap(
        ([key, schema], index) => {
          const annotation =
            Schema.resolveAnnotationsKey(schema) ?? Schema.resolveAnnotations(schema);
          const form = annotation?.providerSettingsForm ?? {};
          if (form.hidden) return [];
          if (form.control && !["text", "password", "select"].includes(form.control)) {
            throw new Error(`Port the new ${driver.driverKind}.${key} control: ${form.control}`);
          }
          const samples = [
            ...new Set([
              "",
              " ",
              "sample",
              "99999",
              "100000",
              "0300000",
              "300000",
              "999999",
              "1000000",
              "1000001",
              ...(form.options ?? []).map((option) => option.value),
            ]),
          ];
          const decode = Schema.decodeUnknownOption(schema);
          return [
            {
              key,
              label: annotation?.title ?? key,
              description: annotation?.description ?? "",
              placeholder: form.placeholder ?? "",
              secret: isSecretSetting(schema),
              control: form.control ?? "text",
              acceptsSecretReference: acceptsSecretReference(schema),
              validation: Object.fromEntries(
                samples.map((value) => [value, decode(value)._tag === "Some"]),
              ),
              persist: form.clearWhenEmpty === "persist",
              options: (form.options ?? []).map((option) => [option.value, option.label]),
              index,
            },
          ];
        },
      );
      fields.sort(
        (a, b) =>
          (order.includes(a.key) ? order.indexOf(a.key) : order.length + a.index) -
          (order.includes(b.key) ? order.indexOf(b.key) : order.length + b.index),
      );
      return [driver.driverKind, fields.map(({ index: _, ...field }) => field)];
    }),
  );
}
