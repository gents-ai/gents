import { pathToFileURL } from "node:url";
import { HostEnvironment } from "./host-environment.mjs";

// Process boundary for the Rust eval coordinator, not a model-facing tool.
export async function control(argv, env = process.env) {
  const [operation, id, fault] = argv;
  if (operation === "fork") {
    if (argv.length !== 3)
      throw new Error("fork requires container and private snapshot directory");
    const original = new HostEnvironment(id);
    const candidate = await original.forkStoppedRuntime({
      endpoint: env.GENTS_D4F_ENDPOINT,
      directory: fault,
    });
    try {
      return {
        container_id: candidate.id,
        original_container_id: original.id,
        graphql: await candidate.startRuntime(),
        host: await candidate.snapshot(),
      };
    } catch (error) {
      await candidate.close();
      throw error;
    }
  }
  if (operation === "start") {
    if (argv.length !== 1)
      throw new Error("start takes no positional arguments");
    const host = await HostEnvironment.start({
      runtime: true,
      endpoint: env.GENTS_D4F_ENDPOINT,
      runtimeImage: env.GENTS_HOST_RUNTIME_IMAGE,
    });
    try {
      const graphql = await host.provision({
        endpoint: env.GENTS_D4F_ENDPOINT,
        model: env.GENTS_D4F_MODEL,
      });
      return {
        container_id: host.id,
        runtime_image: env.GENTS_HOST_RUNTIME_IMAGE,
        graphql,
        host: await host.snapshot(),
      };
    } catch (error) {
      await host.close();
      throw error;
    }
  }
  if (operation === "submit") {
    if (argv.length !== 6)
      throw new Error(
        "submit requires container, behavior, session, prompt, timeout",
      );
    const host = new HostEnvironment(id);
    await host.assertOwned();
    await host.submitChat(argv[2], argv[3], argv[4], Number(argv[5]));
    return { submitted_session: argv[3] };
  }
  if (
    ![
      "snapshot",
      "fault",
      "restart",
      "resume",
      "archive",
      "close",
      "restore",
      "dismiss",
    ].includes(operation)
  )
    throw new Error(`Unknown host operation: ${operation}`);
  if (
    argv.length !==
    (["fault", "archive", "dismiss"].includes(operation) ? 3 : 2)
  )
    throw new Error("Incorrect host operation arguments");
  const host = new HostEnvironment(id);
  await host.assertOwned();
  switch (operation) {
    case "restore":
      await host.restoreMonitoringFaults();
      return host.snapshot();
    case "dismiss":
      await host.dismiss(fault);
      return { dismissed: true };
    case "snapshot":
      return host.snapshot();
    case "fault":
      await host.inject(fault);
      return host.snapshot();
    case "restart":
    case "resume":
      if (operation === "restart") await host.stopRuntime();
      return {
        graphql: await host.startRuntime(),
        host: await host.snapshot(),
      };
    case "close":
      await host.close();
      return { closed: true };
    case "archive":
      await host.archiveRuntime(fault);
      return { archived: true };
  }
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  try {
    process.stdout.write(
      `${JSON.stringify(await control(process.argv.slice(2)))}\n`,
    );
  } catch (error) {
    process.stderr.write(`${error.stack || error}\n`);
    process.exitCode = 1;
  }
}
