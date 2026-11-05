use crate::expectation::CollectionExpectation;

#[derive(Debug, Clone, Copy)]
pub struct BaselineCollection<'a> {
    pub name: &'a str,
    pub sdl: &'a str,
    pub expected_version: Option<&'a str>,
    pub expected_state: CollectionExpectation,
}

#[derive(Debug, Clone, Copy)]
pub struct LensSpec<'a> {
    pub wasm: &'a [u8],
    pub args_json: Option<&'a str>,
}

#[derive(Debug, Clone, Copy)]
pub enum MigrationStep<'a> {
    AddCollection {
        id: &'a str,
        sdl: &'a str,
        expected_version: Option<&'a str>,
        expected_state: CollectionExpectation,
    },
    PatchVersioned {
        id: &'a str,
        collection: &'a str,
        patch: &'a str,
        lens: Option<LensSpec<'a>>,
        expected_version: Option<&'a str>,
        expected_transform: Option<&'a str>,
        expected_state: CollectionExpectation,
    },
    PatchInPlace {
        id: &'a str,
        collection: &'a str,
        patch: &'a str,
        expected_state: CollectionExpectation,
    },
}

impl<'a> MigrationStep<'a> {
    pub fn id(&self) -> &'a str {
        match self {
            Self::AddCollection { id, .. }
            | Self::PatchVersioned { id, .. }
            | Self::PatchInPlace { id, .. } => id,
        }
    }

    pub fn collection(&self) -> Option<&'a str> {
        match self {
            Self::AddCollection { .. } => None,
            Self::PatchVersioned { collection, .. } | Self::PatchInPlace { collection, .. } => {
                Some(*collection)
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Registry<'a> {
    pub baseline: &'a [BaselineCollection<'a>],
    pub steps: &'a [MigrationStep<'a>],
    pub ordinary_indexes: &'a [OrdinaryIndex<'a>],
}

#[derive(Debug, Clone, Copy)]
pub struct OrdinaryIndex<'a> {
    pub collection: &'a str,
    pub name: &'a str,
    pub fields: &'a [OrdinaryIndexField<'a>],
}

#[derive(Debug, Clone, Copy)]
pub struct OrdinaryIndexField<'a> {
    pub name: &'a str,
    pub descending: bool,
}

impl<'a> Registry<'a> {
    pub fn managed_names(&self) -> impl Iterator<Item = &'a str> + '_ {
        self.baseline.iter().map(|b| b.name)
    }
}

#[derive(Debug, Clone)]
pub struct BaselineCollectionOwned {
    pub name: String,
    pub sdl: String,
    pub expected_version: Option<String>,
    pub expected_state: CollectionExpectation,
}

#[derive(Debug, Clone)]
pub struct LensSpecOwned {
    pub wasm: Vec<u8>,
    pub args_json: Option<String>,
}

#[derive(Debug, Clone)]
pub enum MigrationStepOwned {
    AddCollection {
        id: String,
        sdl: String,
        expected_version: Option<String>,
        expected_state: CollectionExpectation,
    },
    PatchVersioned {
        id: String,
        collection: String,
        patch: String,
        lens: Option<LensSpecOwned>,
        expected_version: Option<String>,
        expected_transform: Option<String>,
        expected_state: CollectionExpectation,
    },
    PatchInPlace {
        id: String,
        collection: String,
        patch: String,
        expected_state: CollectionExpectation,
    },
}

#[derive(Debug, Clone, Default)]
pub struct DynamicRegistry {
    pub baseline: Vec<BaselineCollectionOwned>,
    pub steps: Vec<MigrationStepOwned>,
}

impl DynamicRegistry {
    pub fn as_registry(&self) -> (Vec<BaselineCollection<'_>>, Vec<MigrationStep<'_>>) {
        let baseline = self
            .baseline
            .iter()
            .map(|b| BaselineCollection {
                name: b.name.as_str(),
                sdl: b.sdl.as_str(),
                expected_version: b.expected_version.as_deref(),
                expected_state: b.expected_state,
            })
            .collect();
        let steps = self
            .steps
            .iter()
            .map(|s| match s {
                MigrationStepOwned::AddCollection {
                    id,
                    sdl,
                    expected_version,
                    expected_state,
                } => MigrationStep::AddCollection {
                    id: id.as_str(),
                    sdl: sdl.as_str(),
                    expected_version: expected_version.as_deref(),
                    expected_state: *expected_state,
                },
                MigrationStepOwned::PatchVersioned {
                    id,
                    collection,
                    patch,
                    lens,
                    expected_version,
                    expected_transform,
                    expected_state,
                } => MigrationStep::PatchVersioned {
                    id: id.as_str(),
                    collection: collection.as_str(),
                    patch: patch.as_str(),
                    lens: lens.as_ref().map(|l| LensSpec {
                        wasm: l.wasm.as_slice(),
                        args_json: l.args_json.as_deref(),
                    }),
                    expected_version: expected_version.as_deref(),
                    expected_transform: expected_transform.as_deref(),
                    expected_state: *expected_state,
                },
                MigrationStepOwned::PatchInPlace {
                    id,
                    collection,
                    patch,
                    expected_state,
                } => MigrationStep::PatchInPlace {
                    id: id.as_str(),
                    collection: collection.as_str(),
                    patch: patch.as_str(),
                    expected_state: *expected_state,
                },
            })
            .collect();
        (baseline, steps)
    }
}

macro_rules! baseline_entry {
    ($name:expr, $sdl:expr, $version:literal) => {
        BaselineCollection {
            name: $name,
            sdl: $sdl,
            expected_version: Some($version),
            expected_state: CollectionExpectation::dag_only(),
        }
    };
}

pub static DEFAULT_BASELINE: &[BaselineCollection<'static>] = &[
    // `InferenceBackend` retains its rc0 root
    // (`upgrades::CANONICAL_ROOTS`): a fresh store registers it and takes the same patches an
    // upgraded rc0 store takes, so every store converges on one version.
    baseline_entry!(
        gents_protocol::schemas::INFERENCE_BACKEND_NAME,
        crate::upgrades::BACKEND_RC0_SDL,
        "bafyreige56taagffni2nkys46sezgidvf3wf3jbov6jmqsfsaae3f6oike"
    ),
    baseline_entry!(
        gents_protocol::schemas::NODE_NAME,
        gents_protocol::schemas::NODE,
        "bafyreigbr2zy7awxue5szey4uic2cj4yjiak73equdlovg5hreneojbkae"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_NAME,
        gents_protocol::schemas::AGENT,
        "bafyreihy55y7dycjquzn2zv3y2idmorr42w2qhkpgyfjjgat42oowj2q34"
    ),
    baseline_entry!(
        gents_protocol::schemas::COMPACTION_CONFIG_NAME,
        gents_protocol::schemas::COMPACTION_CONFIG,
        "bafyreiggbqysnj3to7zcw3534eah7ulx5fyfyqfq3le75cli6nf527r2ce"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_CONTEXT_NAME,
        gents_protocol::schemas::AGENT_CONTEXT,
        "bafyreigvm34xfkhx34jvxyskcreltg2rbmb7n6evuok44gxqrijoaw7lti"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_TARGET_NAME,
        gents_protocol::schemas::AGENT_TARGET,
        "bafyreifwgkcyevp2whmcumgnpwsihc5g3hn62bm6idnfynj6t23b3ahdye"
    ),
    baseline_entry!(
        gents_protocol::schemas::NODE_RUNTIME_NAME,
        gents_protocol::schemas::NODE_RUNTIME,
        "bafyreif7je44mc6mdxtpp6wpakbdhuzsjibgtifxlo6pzgfhf2sqlcdrgq"
    ),
    baseline_entry!(
        gents_protocol::schemas::NODE_READINESS_NAME,
        gents_protocol::schemas::NODE_READINESS,
        "bafyreiebc2rek6mqwwmdaglonxkk5qqpsfvv7frbocxb6oufqezvjkct2a"
    ),
    baseline_entry!(
        gents_protocol::schemas::NODE_DIRECTORY_ENTRY_NAME,
        gents_protocol::schemas::NODE_DIRECTORY_ENTRY,
        "bafyreig5c5e56nzpihh44ouahzyhnu76qihzwmde6z77yahlk4cyurqppu"
    ),
    baseline_entry!(
        gents_protocol::schemas::NODE_MEMORY_NAME,
        gents_protocol::schemas::NODE_MEMORY,
        "bafyreich7rj5lgj3ce5mfndp7xy4awh2khblveen7c7ihf6djm5j2dpo5m"
    ),
    baseline_entry!(
        gents_protocol::schemas::TOOLS_NAME,
        gents_protocol::schemas::TOOLS,
        "bafyreiay45qi2chkcus5wfycrsumq5vr6igrydc3ebsqqr2qrkb3qfgo6a"
    ),
    baseline_entry!(
        gents_protocol::schemas::SKILL_NAME,
        gents_protocol::schemas::SKILL,
        "bafyreidhsmcxsmklefypyp7uaig5tgefwvpt4dv6nurgy7rbxlj3egpwla"
    ),
    baseline_entry!(
        gents_protocol::schemas::DATASTORE_TOOL_SURFACE_NAME,
        gents_protocol::schemas::DATASTORE_TOOL_SURFACE,
        "bafyreibhujbx2v6qqrp3l3iqg2j5ph6ggckf5r263ykmsks7dnodjygope"
    ),
    baseline_entry!(
        gents_protocol::schemas::PENDING_CHAT_SUBMISSION_NAME,
        gents_protocol::schemas::PENDING_CHAT_SUBMISSION,
        "bafyreib3pvpyumkvpovhi2x5ua34wzwsvj6o2iz7fge3qwsp2nj7oask44"
    ),
    baseline_entry!(
        gents_protocol::schemas::WORKSPACE_ROOT_NAME,
        gents_protocol::schemas::WORKSPACE_ROOT,
        "bafyreibw7kuk4xise6epukrca2inza3j44bgsxfbkse3tkbk6enqzsr6ui"
    ),
    baseline_entry!(
        gents_protocol::schemas::ISOLATED_WORKSPACE_NAME,
        gents_protocol::schemas::ISOLATED_WORKSPACE,
        "bafyreigqhbaoq4ttjzt6cwi7p43tktv4urviml22zmmpzzvndfr7sh5afy"
    ),
    baseline_entry!(
        gents_protocol::schemas::WORKSPACE_PLACEMENT_NAME,
        gents_protocol::schemas::WORKSPACE_PLACEMENT,
        "bafyreidhu4e5ddjund4nzwlw4hxo3k5vwi6eidwtnsyxf345wef7ffbv2i"
    ),
    baseline_entry!(
        gents_protocol::schemas::REPOSITORY_PLACEMENT_NAME,
        gents_protocol::schemas::REPOSITORY_PLACEMENT,
        "bafyreieks33oeixd4h4sdqfmli3fyuksjo3bkw7povukuak6xvavl7sbxq"
    ),
    baseline_entry!(
        gents_protocol::schemas::WORKSPACE_BINDING_NAME,
        gents_protocol::schemas::WORKSPACE_BINDING,
        "bafyreigofvbludczwhf5dm5kua4e5jhkzrtf3fgvktk6p5k75utwhdts2y"
    ),
    baseline_entry!(
        gents_protocol::schemas::WORKSPACE_RECEIPT_NAME,
        gents_protocol::schemas::WORKSPACE_RECEIPT,
        "bafyreiecsdzgs6zctavbmm7hom5saefv3k23qmrfmnbeguriji346ofnnq"
    ),
    baseline_entry!(
        gents_protocol::schemas::CALLBACK_NAME,
        gents_protocol::schemas::CALLBACK,
        "bafyreihet57wzgngc6iylbyga23pnzpgueos5sgz36vkg4uzc3hpn6kgmi"
    ),
    baseline_entry!(
        gents_protocol::schemas::EVENT_SOURCE_NAME,
        gents_protocol::schemas::EVENT_SOURCE,
        "bafyreidbvp5dtcm2frjhmb7ymgkl76egaxa4qcgour4j3xoys7ppc5umai"
    ),
    baseline_entry!(
        gents_protocol::schemas::TRIGGER_NAME,
        gents_protocol::schemas::TRIGGER,
        "bafyreiez56udcss6hqfibh4j5fxfhbac7wp63y7pfz77vwvqz2fqrt5f74"
    ),
    baseline_entry!(
        gents_protocol::schemas::EVENT_SOURCE_CURSOR_NAME,
        gents_protocol::schemas::EVENT_SOURCE_CURSOR,
        "bafyreichgs26urdud6sk4qualskmzpcodc5o2mlljnswcxmleihz2vo2qq"
    ),
    baseline_entry!(
        gents_protocol::schemas::TRIGGER_FIRE_NAME,
        gents_protocol::schemas::TRIGGER_FIRE,
        "bafyreibgl6fpygvxtcgwmdk2wgufhmujonq4p7e2muvy23f6nadeyolzle"
    ),
    baseline_entry!(
        gents_protocol::schemas::FIRE_OUTCOME_NAME,
        gents_protocol::schemas::FIRE_OUTCOME,
        "bafyreiebsekkoog62amjydsetyiky2rgiritfyphcqn7mvgskjaylw2h5m"
    ),
    baseline_entry!(
        gents_protocol::schemas::CALLBACK_MODULE_NAME,
        gents_protocol::schemas::CALLBACK_MODULE,
        "bafyreihyynxed2unvgkpm3cypzzodcm2ryy3q4hd6wghks5ur74b4hd73i"
    ),
    baseline_entry!(
        gents_protocol::schemas::CALLBACK_BINDING_NAME,
        gents_protocol::schemas::CALLBACK_BINDING,
        "bafyreifelknkrxmguojv547qxpqn2y62s5h23wrqbfupugmocfhrxgowma"
    ),
    baseline_entry!(
        gents_protocol::schemas::CALLBACK_INVOCATION_NAME,
        gents_protocol::schemas::CALLBACK_INVOCATION,
        "bafyreifc4guq64whykqkw3qk4bvvkrvwin7g2swpvzppyudtc62btboryq"
    ),
    baseline_entry!(
        gents_protocol::schemas::CALLBACK_RESULT_NAME,
        gents_protocol::schemas::CALLBACK_RESULT,
        "bafyreiay7i7erfmg4o56oqrzbbwrohw253x7yme4plut46t4tgpzujdcq4"
    ),
    baseline_entry!(
        gents_protocol::schemas::OAUTH_CREDENTIAL_NAME,
        gents_protocol::schemas::OAUTH_CREDENTIAL,
        "bafyreia6tt5aickbpdovdivyhzq7xfidy4s5mpa7sqlksq6etpw5w6xjdm"
    ),
    baseline_entry!(
        gents_protocol::schemas::INFERENCE_PROFILE_NAME,
        gents_protocol::schemas::INFERENCE_PROFILE,
        "bafyreidrxjg2xkpyyoi7k4szrvxojjmjg2vhzqlfwb2av4m65yvywimj3m"
    ),
    baseline_entry!(
        gents_protocol::schemas::INFERENCE_RETRY_POLICY_NAME,
        gents_protocol::schemas::INFERENCE_RETRY_POLICY,
        "bafyreiasnka6siox245v35twdscltsap7wigaej2ytjgf6tleoonxnsvlm"
    ),
    baseline_entry!(
        gents_protocol::schemas::INFERENCE_EXECUTION_NAME,
        gents_protocol::schemas::INFERENCE_EXECUTION,
        "bafyreig4qstxcthbe4r56v3saqtvidikn5ymrjunyt2ofvmc5g7duqhsea"
    ),
    baseline_entry!(
        gents_protocol::schemas::INFERENCE_SAMPLING_NAME,
        gents_protocol::schemas::INFERENCE_SAMPLING,
        "bafyreicf7mrn3mfyu7smgy6vx4huo6iu6cj7j5cuuz2czmwj2gctfnjppq"
    ),
    baseline_entry!(
        gents_protocol::schemas::INFERENCE_CALL_NAME,
        gents_protocol::schemas::INFERENCE_CALL,
        "bafyreid5bt6u5a25rdvwpaaz53nvqaqnjx2h53iqli5v627ov2vbvuopo4"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_REQUEST_NAME,
        gents_protocol::schemas::AGENT_REQUEST,
        "bafyreiffadcnnzmv6ebbqx6aw3kweylg4d6n5ci6eloyepflysyqbur4ze"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_OUTPUT_SEGMENT_NAME,
        gents_protocol::schemas::AGENT_OUTPUT_SEGMENT,
        "bafyreid5flveud445po2hzcpiyluq3mjh5plyz5qxg7aj5r6mkgaqqwaci"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_SESSION_NAME,
        gents_protocol::schemas::AGENT_SESSION,
        "bafyreifmtywrh7eccrsayybrec6guazd2s4v7dfxrrf6xsva66cz3mt5ze"
    ),
    baseline_entry!(
        gents_protocol::schemas::GOAL_NAME,
        gents_protocol::schemas::GOAL,
        "bafyreihfzjt7jgymk2mgdgienxz3b4qimhv4iwv3zhk6e3n2mzutxjd53e"
    ),
    baseline_entry!(
        gents_protocol::schemas::GOAL_CREATION_CLAIM_NAME,
        gents_protocol::schemas::GOAL_CREATION_CLAIM,
        "bafyreibspclsdz7xbbhexxxqjydom4azb2zkytvekwxfz2jwz7dhrwgr6q"
    ),
    baseline_entry!(
        gents_protocol::schemas::MAILBOX_ITEM_NAME,
        gents_protocol::schemas::MAILBOX_ITEM,
        "bafyreidcjjvuogdcjerncm3cjbeuh2iiai75b2ylq2lwtoggdkh3ytquzm"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_MESSAGE_NAME,
        gents_protocol::schemas::AGENT_MESSAGE,
        "bafyreidgfzxcgne524l6c5d4vpflff3nzichjuucvuwqqw37neqcabmd7m"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_TOOL_CALL_NAME,
        gents_protocol::schemas::AGENT_TOOL_CALL,
        "bafyreibf2m33rcw2zqsknyp2jut7bzeyv7tudb2gj6yjrctgnxvid4i2te"
    ),
    baseline_entry!(
        gents_protocol::schemas::TOOL_FILE_CHANGE_NAME,
        gents_protocol::schemas::TOOL_FILE_CHANGE,
        "bafyreihw4weuomlxjf2imd46bw5mgcpbfaicnbjiuqhs5yo6tvaam4prae"
    ),
    baseline_entry!(
        gents_protocol::schemas::TOOL_FILE_CONTENT_NAME,
        gents_protocol::schemas::TOOL_FILE_CONTENT,
        "bafyreibqziwhiq4dxkeqfeokkt22yajmnimxegww4lbr3r7k57b6dvetby"
    ),
    baseline_entry!(
        gents_protocol::schemas::TOOL_FILE_INVENTORY_NAME,
        gents_protocol::schemas::TOOL_FILE_INVENTORY,
        "bafyreifcfokctbxkg54oujluu7mjepz4p56lnrmkzj36ng34glaaxg4vuq"
    ),
    baseline_entry!(
        gents_protocol::schemas::COMPACTION_ENTRY_NAME,
        gents_protocol::schemas::COMPACTION_ENTRY,
        "bafyreidfp36wuikilut4cnvs7qwmqbyyd5yyyaz5g7umk2lfqwzqojgn4q"
    ),
    baseline_entry!(
        gents_protocol::schemas::RENDERED_REQUEST_NAME,
        gents_protocol::schemas::RENDERED_REQUEST,
        "bafyreigittjfc7aawco37kkfwz5lirou5slz5zqvqsknosaltpuvn5jucm"
    ),
    baseline_entry!(
        gents_protocol::schemas::RENDERED_REQUEST_BLOCK_NAME,
        gents_protocol::schemas::RENDERED_REQUEST_BLOCK,
        "bafyreicl3b2r4ealg4l5dwfwdefsgtv4su7obbibgnmmu3iomaieeshaa4"
    ),
    baseline_entry!(
        gents_protocol::schemas::PROVIDER_CONTEXT_REDUCTION_NAME,
        gents_protocol::schemas::PROVIDER_CONTEXT_REDUCTION,
        "bafyreidzlfqy2l2l7puqzlswpi7ychcgukdaaii6qq4r7i5y46fs2loi2q"
    ),
    baseline_entry!(
        gents_protocol::schemas::PROJECTION_ACP_BINDING_NAME,
        gents_protocol::schemas::PROJECTION_ACP_BINDING,
        "bafyreigykdnbn4ouyvgkvfzvsir5d6l3gwgng46wvhnwrws3rhfo32ug6a"
    ),
    baseline_entry!(
        gents_protocol::schemas::TASK_NAME,
        gents_protocol::schemas::TASK,
        "bafyreieh67b32auhf33bkdltep2gh6gmrb5wgs2sd7vxexmqxno45jnmdm"
    ),
    baseline_entry!(
        gents_protocol::schemas::SCHEDULE_NAME,
        gents_protocol::schemas::SCHEDULE,
        "bafyreibi3hlloxszn4lgipphx55nksivy5x3orhfhshft6qzr64ghedf5a"
    ),
    baseline_entry!(
        gents_protocol::schemas::EVENT_GROUP_STATE_NAME,
        gents_protocol::schemas::EVENT_GROUP_STATE,
        "bafyreids6vkxatjttmedkypqlibwbyyebyapq5p5vrn7dlfyh5iqbzrq6y"
    ),
    baseline_entry!(
        gents_protocol::schemas::TOOL_SERVICE_REGISTRY_NAME,
        gents_protocol::schemas::TOOL_SERVICE_REGISTRY,
        "bafyreigckps33q33zvnrodzqlz72ri4osyodxleniwy6txwkfhnq7tgg3q"
    ),
    baseline_entry!(
        gents_protocol::schemas::TOOL_SERVICE_HEALTH_STATE_NAME,
        gents_protocol::schemas::TOOL_SERVICE_HEALTH_STATE,
        "bafyreiatdha4wto462443nnyh4ic35t7xsgc5pvo6jrbrppx3pxsvpgehi"
    ),
    baseline_entry!(
        gents_protocol::schemas::PEER_PAIRING_DESIRED_NAME,
        gents_protocol::schemas::PEER_PAIRING_DESIRED,
        "bafyreiagpv74772zsb3n7y6dxqbv3tit5hactyrtxk73occf6sxxagw7he"
    ),
    baseline_entry!(
        gents_protocol::schemas::DATA_PLANE_PAIRING_DESIRED_NAME,
        gents_protocol::schemas::DATA_PLANE_PAIRING_DESIRED,
        "bafyreiaitj36nr5nmsevxytznad44gg3bmunlbodwfpq6xueleofu67dpm"
    ),
    baseline_entry!(
        gents_protocol::schemas::PEER_PAIRING_APPLIED_NAME,
        gents_protocol::schemas::PEER_PAIRING_APPLIED,
        "bafyreickzo4uxque2a7wdnkp45ya3bsn33vbs4rpj6xglvxrftsxbd3hla"
    ),
    baseline_entry!(
        gents_protocol::schemas::PEER_REGISTRY_NAME,
        gents_protocol::schemas::PEER_REGISTRY,
        "bafyreihnmzcnyf7lj7lbt4ptxfljxnpxojpp576a45nzdlm4fzrdcyv6du"
    ),
    baseline_entry!(
        gents_protocol::schemas::NETWORK_NAME,
        gents_protocol::schemas::NETWORK,
        "bafyreighwrxb6enx4wppybg6qycvrikw3ph3zelexnzcnyujgpykpkl6pi"
    ),
    baseline_entry!(
        gents_protocol::schemas::NETWORK_ADMIN_PIN_NAME,
        gents_protocol::schemas::NETWORK_ADMIN_PIN,
        "bafyreihqlke25nkquhgf3jiokj26dz2gc4do62o3odhw3iwcfbsofy5iki"
    ),
    baseline_entry!(
        gents_protocol::schemas::NETWORK_ENROLLMENT_REQUEST_NAME,
        gents_protocol::schemas::NETWORK_ENROLLMENT_REQUEST,
        "bafyreifqpuwotuu6x7j7zdqlhnsnf6llylk3fo47dl6bopwz2tslz7hwci"
    ),
    baseline_entry!(
        gents_protocol::schemas::NETWORK_ENROLLMENT_DECISION_NAME,
        gents_protocol::schemas::NETWORK_ENROLLMENT_DECISION,
        "bafyreihf3frxgpimoon66spsdvpt27uqnyqs3gnsmaq7rwdvtuslvabpxa"
    ),
    baseline_entry!(
        gents_protocol::schemas::NETWORK_AUTHORIZATION_REVISION_NAME,
        gents_protocol::schemas::NETWORK_AUTHORIZATION_REVISION,
        "bafyreifn25bep4sol5de3f7immtnekl7fyhkgeo7dxi4433ncj53bukgvu"
    ),
    baseline_entry!(
        gents_protocol::schemas::NETWORK_ENROLLMENT_ROUTE_RECEIPT_NAME,
        gents_protocol::schemas::NETWORK_ENROLLMENT_ROUTE_RECEIPT,
        "bafyreied4f7ouk7ymrud44xrdl3mlaqnga3z4p5rhlu5ibdzozwsufy54i"
    ),
    baseline_entry!(
        gents_protocol::schemas::ENROLLMENT_OPERATOR_NONCE_NAME,
        gents_protocol::schemas::ENROLLMENT_OPERATOR_NONCE,
        "bafyreihqgfh2e6gc73zftjp6d2qlyba7hxsc3ab6mhgfb7jn4yydvdihaq"
    ),
    baseline_entry!(
        gents_protocol::schemas::SESSION_HYDRATION_REQUEST_NAME,
        gents_protocol::schemas::SESSION_HYDRATION_REQUEST,
        "bafyreiebw7lxwzdcmltfz6khjz6e32utfpfwpxydnqwcnnsbtj4qqhxcmq"
    ),
    baseline_entry!(
        gents_protocol::schemas::FILE_IMAGE_HYDRATION_REQUEST_NAME,
        gents_protocol::schemas::FILE_IMAGE_HYDRATION_REQUEST,
        "bafyreihbcklqo55343lt7spcif4xftwqbsfe6flznvpge3sfcre3sakdgu"
    ),
    baseline_entry!(
        gents_protocol::schemas::FILE_INVENTORY_IMAGE_REQUEST_NAME,
        gents_protocol::schemas::FILE_INVENTORY_IMAGE_REQUEST,
        "bafyreidafcfcocovwduklm6rjppadnof4t3bndhkmrqoqe55imm447ox3i"
    ),
    baseline_entry!(
        gents_protocol::schemas::FILE_INVENTORY_REQUEST_NAME,
        gents_protocol::schemas::FILE_INVENTORY_REQUEST,
        "bafyreie5fnoj47pu4shae63v7gx4mnxcovmrs5kttnries67sb2ftgk6rm"
    ),
    baseline_entry!(
        gents_protocol::schemas::FILE_INVENTORY_PAGE_NAME,
        gents_protocol::schemas::FILE_INVENTORY_PAGE,
        "bafyreiaf5sdqtuiursuwu5djfotcmqcx6zlhkrsmrj3kyea6pntrzabbsu"
    ),
    baseline_entry!(
        gents_protocol::schemas::FILE_INVENTORY_REPLY_NAME,
        gents_protocol::schemas::FILE_INVENTORY_REPLY,
        "bafyreicj5rsouhjk2j36qigmwamnlyh6xqzwttvdhvvcxkkdsmzgwnnsei"
    ),
    baseline_entry!(
        gents_protocol::schemas::FILE_INVENTORY_PROGRESS_NAME,
        gents_protocol::schemas::FILE_INVENTORY_PROGRESS,
        "bafyreifvr5ggapacp6y7b7vg2cmlwbkqmhbwia5julkp5cwa2x44drseye"
    ),
    baseline_entry!(
        gents_protocol::schemas::FILE_IMAGE_HYDRATION_RECEIPT_NAME,
        gents_protocol::schemas::FILE_IMAGE_HYDRATION_RECEIPT,
        "bafyreickb6g5druozipch55cjpaciwvm5gafgtrprqmdtezp4zqddk5fry"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_SESSION_INPUT_EDIT_NAME,
        gents_protocol::schemas::AGENT_SESSION_INPUT_EDIT,
        "bafyreiayfxrn7hgy6xmovsmhlenwoyyb22px63mo3cjogsp4u2dapo36oe"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_SESSION_GOAL_CONTROL_NAME,
        gents_protocol::schemas::AGENT_SESSION_GOAL_CONTROL,
        "bafyreid4zq25mkxrp4p4ttkfmrhcgaviey5fjgtou2dpqvt6la4zs5wm3q"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_SESSION_GOAL_RECEIPT_NAME,
        gents_protocol::schemas::AGENT_SESSION_GOAL_RECEIPT,
        "bafyreibbuxtqsh3tbkneq5satxfcadwrquwfaufgo2y5vqax32sdu4lkjq"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_TOOL_CONTROL_NAME,
        gents_protocol::schemas::AGENT_TOOL_CONTROL,
        "bafyreihmzxo4teggqcgqznmbnvdrobtwhl3smv3mcd477zs4q3d3ge6tou"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_TOOL_CONTROL_CLAIM_NAME,
        gents_protocol::schemas::AGENT_TOOL_CONTROL_CLAIM,
        "bafyreig3ogmqcnykgwz5czfhfxwcb6vfw7ufuga2zc3q66wt23ns3uan5u"
    ),
    baseline_entry!(
        gents_protocol::schemas::GOAL_OBSERVATION_NAME,
        gents_protocol::schemas::GOAL_OBSERVATION,
        "bafyreie7hlgybltivigtqmufnmuuvnhdl5ohtwd52qi7fcsqyd6ftutk5q"
    ),
    baseline_entry!(
        gents_protocol::schemas::EVAL_DEFINITION_NAME,
        gents_protocol::schemas::EVAL_DEFINITION,
        "bafyreibeokxszqypuejna3pckyp36vujfpfg2yahl3y3u3scpngdbigjtm"
    ),
    baseline_entry!(
        gents_protocol::schemas::EVAL_RUN_NAME,
        gents_protocol::schemas::EVAL_RUN,
        "bafyreihebflrtxgdxkjwt7wr3ep2vaej7arrsqzbqcl5z7zeahqoekdsqi"
    ),
    baseline_entry!(
        gents_protocol::schemas::EVAL_TRIAL_NAME,
        gents_protocol::schemas::EVAL_TRIAL,
        "bafyreic5i7efn5ygbpnaarkkhji6sjuojihzamf75tcypp4fptkkn6enum"
    ),
    baseline_entry!(
        gents_protocol::schemas::EVAL_VERDICT_NAME,
        gents_protocol::schemas::EVAL_VERDICT,
        "bafyreic3hxrxirhln4b2mnsjmeuz73oycdjw6dnfpzfz3qogzq6anbbzbe"
    ),
    baseline_entry!(
        gents_protocol::schemas::OPTIMIZATION_JOB_NAME,
        gents_protocol::schemas::OPTIMIZATION_JOB,
        "bafyreiamgjslitr3wvsrdcgqp75onccmpetjbi4nq4jwxgexbxyfaqwp7m"
    ),
    baseline_entry!(
        gents_protocol::schemas::PACK_INSTALLATION_NAME,
        gents_protocol::schemas::PACK_INSTALLATION,
        "bafyreia5f6iex3vrzfjzzlpm7jqkqehmnis3vqnbcdyuno2jlafyyrx3vu"
    ),
];

pub static DEFAULT_STEPS: &[MigrationStep<'static>] = &[];

static AGENT_REQUEST_NODE_SESSION_FIELDS: &[OrdinaryIndexField<'static>] = &[
    OrdinaryIndexField {
        name: "node_did",
        descending: false,
    },
    OrdinaryIndexField {
        name: "session_id",
        descending: false,
    },
];

static AGENT_REQUEST_TRIGGER_HISTORY_FIELDS: &[OrdinaryIndexField<'static>] = &[
    OrdinaryIndexField {
        name: "node_did",
        descending: false,
    },
    OrdinaryIndexField {
        name: "caused_by_trigger_id",
        descending: false,
    },
    OrdinaryIndexField {
        name: "purpose",
        descending: false,
    },
    OrdinaryIndexField {
        name: "created_at",
        descending: true,
    },
    OrdinaryIndexField {
        name: "request_id",
        descending: true,
    },
    OrdinaryIndexField {
        name: "session_id",
        descending: false,
    },
];

static AGENT_REQUEST_PREDECESSOR_FIELDS: &[OrdinaryIndexField<'static>] = &[
    OrdinaryIndexField {
        name: "request_id",
        descending: false,
    },
    OrdinaryIndexField {
        name: "node_did",
        descending: false,
    },
    OrdinaryIndexField {
        name: "session_id",
        descending: false,
    },
    OrdinaryIndexField {
        name: "requester_did",
        descending: false,
    },
];

static AGENT_REQUEST_RETRY_CHILD_FIELDS: &[OrdinaryIndexField<'static>] = &[
    OrdinaryIndexField {
        name: "retry_parent_request",
        descending: false,
    },
    OrdinaryIndexField {
        name: "node_did",
        descending: false,
    },
    OrdinaryIndexField {
        name: "session_id",
        descending: false,
    },
    OrdinaryIndexField {
        name: "requester_did",
        descending: false,
    },
];

static MAILBOX_NODE_STATUS_ITEM_KEY_FIELDS: &[OrdinaryIndexField<'static>] = &[
    OrdinaryIndexField {
        name: "node_did",
        descending: false,
    },
    OrdinaryIndexField {
        name: "status",
        descending: false,
    },
    OrdinaryIndexField {
        name: "item_key",
        descending: false,
    },
];

static AGENT_OUTPUT_SOURCE_FIELDS: &[OrdinaryIndexField<'static>] = &[
    OrdinaryIndexField {
        name: "request_doc_id",
        descending: false,
    },
    OrdinaryIndexField {
        name: "source",
        descending: false,
    },
];

static MESSAGE_SESSION_SEQUENCE_FIELDS: &[OrdinaryIndexField<'static>] = &[
    OrdinaryIndexField {
        name: "session_id",
        descending: false,
    },
    OrdinaryIndexField {
        name: "sequence",
        descending: false,
    },
];

static TOOL_CALL_SESSION_ORDER_FIELDS: &[OrdinaryIndexField<'static>] = &[
    OrdinaryIndexField {
        name: "session_id",
        descending: false,
    },
    OrdinaryIndexField {
        name: "message_sequence",
        descending: false,
    },
    OrdinaryIndexField {
        name: "tool_call_key",
        descending: false,
    },
];

static TOOL_CALL_BACKGROUND_STATUS_FIELDS: &[OrdinaryIndexField<'static>] = &[
    OrdinaryIndexField {
        name: "node_did",
        descending: false,
    },
    OrdinaryIndexField {
        name: "await_mode",
        descending: false,
    },
    OrdinaryIndexField {
        name: "status",
        descending: false,
    },
];

static SESSION_ARCHIVED_AT_FIELDS: &[OrdinaryIndexField<'static>] = &[OrdinaryIndexField {
    name: "archived_at",
    descending: false,
}];

static SESSION_STOPPED_AT_FIELDS: &[OrdinaryIndexField<'static>] = &[OrdinaryIndexField {
    name: "stopped_at",
    descending: false,
}];

/// The metrics scrape reads terminal inference calls that ended inside its
/// trailing window; without this range every scrape read every call.
static INFERENCE_CALL_TERMINAL_WINDOW_FIELDS: &[OrdinaryIndexField<'static>] = &[
    OrdinaryIndexField {
        name: "call_kind",
        descending: false,
    },
    OrdinaryIndexField {
        name: "ended_at",
        descending: false,
    },
];

static RENDERED_REQUEST_CALL_FIELDS: &[OrdinaryIndexField<'static>] = &[
    OrdinaryIndexField {
        name: "request_doc_id",
        descending: false,
    },
    OrdinaryIndexField {
        name: "node_did",
        descending: false,
    },
    OrdinaryIndexField {
        name: "session_id",
        descending: false,
    },
    OrdinaryIndexField {
        name: "turn_index",
        descending: false,
    },
    OrdinaryIndexField {
        name: "attempt",
        descending: false,
    },
];

pub static DEFAULT_ORDINARY_INDEXES: &[OrdinaryIndex<'static>] = &[
    OrdinaryIndex {
        collection: gents_protocol::schemas::AGENT_SESSION_NAME,
        name: "AgentSession_started_by_node_did_ASC",
        fields: &[OrdinaryIndexField {
            name: "started_by_node_did",
            descending: false,
        }],
    },
    OrdinaryIndex {
        collection: gents_protocol::schemas::AGENT_SESSION_NAME,
        name: "AgentSession_started_by_session_id_ASC",
        fields: &[OrdinaryIndexField {
            name: "started_by_session_id",
            descending: false,
        }],
    },
    OrdinaryIndex {
        collection: gents_protocol::schemas::INFERENCE_CALL_NAME,
        name: "InferenceCall_session_id_ASC",
        fields: &[OrdinaryIndexField {
            name: "session_id",
            descending: false,
        }],
    },
    OrdinaryIndex {
        collection: gents_protocol::schemas::INFERENCE_CALL_NAME,
        name: "InferenceCall_model_name_ASC",
        fields: &[OrdinaryIndexField {
            name: "model_name",
            descending: false,
        }],
    },
    OrdinaryIndex {
        collection: gents_protocol::schemas::INFERENCE_CALL_NAME,
        name: "InferenceCall_profile_id_ASC",
        fields: &[OrdinaryIndexField {
            name: "profile_id",
            descending: false,
        }],
    },
    OrdinaryIndex {
        collection: gents_protocol::schemas::INFERENCE_CALL_NAME,
        name: "InferenceCall_kind_ended_at",
        fields: INFERENCE_CALL_TERMINAL_WINDOW_FIELDS,
    },
    OrdinaryIndex {
        collection: gents_protocol::schemas::AGENT_TOOL_CALL_NAME,
        name: "AgentToolCall_session_sequence_key",
        fields: TOOL_CALL_SESSION_ORDER_FIELDS,
    },
    OrdinaryIndex {
        collection: gents_protocol::schemas::AGENT_MESSAGE_NAME,
        name: "AgentMessage_session_sequence",
        fields: MESSAGE_SESSION_SEQUENCE_FIELDS,
    },
    OrdinaryIndex {
        collection: gents_protocol::schemas::AGENT_TOOL_CALL_NAME,
        name: "AgentToolCall_node_await_status",
        fields: TOOL_CALL_BACKGROUND_STATUS_FIELDS,
    },
    OrdinaryIndex {
        collection: gents_protocol::schemas::AGENT_SESSION_NAME,
        name: "AgentSession_stopped_at_ASC",
        fields: SESSION_STOPPED_AT_FIELDS,
    },
    OrdinaryIndex {
        collection: gents_protocol::schemas::AGENT_SESSION_NAME,
        name: "AgentSession_archived_at_ASC",
        fields: SESSION_ARCHIVED_AT_FIELDS,
    },
    OrdinaryIndex {
        collection: gents_protocol::schemas::AGENT_OUTPUT_SEGMENT_NAME,
        name: "AgentOutputSegment_request_doc_id_source",
        fields: AGENT_OUTPUT_SOURCE_FIELDS,
    },
    OrdinaryIndex {
        collection: gents_protocol::schemas::AGENT_REQUEST_NAME,
        name: "AgentRequest_node_did_session_id",
        fields: AGENT_REQUEST_NODE_SESSION_FIELDS,
    },
    OrdinaryIndex {
        collection: gents_protocol::schemas::AGENT_REQUEST_NAME,
        name: "AgentRequest_node_trigger_created_request",
        fields: AGENT_REQUEST_TRIGGER_HISTORY_FIELDS,
    },
    OrdinaryIndex {
        collection: gents_protocol::schemas::AGENT_REQUEST_NAME,
        name: "AgentRequest_request_node_session_requester",
        fields: AGENT_REQUEST_PREDECESSOR_FIELDS,
    },
    OrdinaryIndex {
        collection: gents_protocol::schemas::AGENT_REQUEST_NAME,
        name: "AgentRequest_retry_parent_node_session_requester",
        fields: AGENT_REQUEST_RETRY_CHILD_FIELDS,
    },
    OrdinaryIndex {
        collection: gents_protocol::schemas::MAILBOX_ITEM_NAME,
        name: "MailboxItem_node_status_item_key",
        fields: MAILBOX_NODE_STATUS_ITEM_KEY_FIELDS,
    },
    OrdinaryIndex {
        collection: gents_protocol::schemas::RENDERED_REQUEST_NAME,
        name: "RenderedRequest_request_node_session_turn_attempt",
        fields: RENDERED_REQUEST_CALL_FIELDS,
    },
];

pub static DEFAULT_REGISTRY: Registry<'static> = Registry {
    baseline: DEFAULT_BASELINE,
    steps: DEFAULT_STEPS,
    ordinary_indexes: DEFAULT_ORDINARY_INDEXES,
};

pub fn fixture_lens_wasm() -> &'static [u8] {
    include_bytes!(env!("GENTS_LENS_FIXTURE_ADD_LABEL_WASM_PATH"))
}

pub const CLIENT_AUTHORED_COLLECTIONS: &[&str] = &[
    gents_protocol::schemas::PEER_REGISTRY_NAME,
    gents_protocol::schemas::AGENT_REQUEST_NAME,
    gents_protocol::schemas::AGENT_MESSAGE_NAME,
    gents_protocol::schemas::AGENT_TOOL_CALL_NAME,
    gents_protocol::schemas::AGENT_OUTPUT_SEGMENT_NAME,
    gents_protocol::schemas::AGENT_SESSION_NAME,
    gents_protocol::schemas::COMPACTION_ENTRY_NAME,
    gents_protocol::schemas::NETWORK_ENROLLMENT_REQUEST_NAME,
    gents_protocol::schemas::NETWORK_ENROLLMENT_DECISION_NAME,
    gents_protocol::schemas::NETWORK_AUTHORIZATION_REVISION_NAME,
    gents_protocol::schemas::NETWORK_ENROLLMENT_ROUTE_RECEIPT_NAME,
    gents_protocol::schemas::SESSION_HYDRATION_REQUEST_NAME,
    gents_protocol::schemas::FILE_IMAGE_HYDRATION_REQUEST_NAME,
    gents_protocol::schemas::FILE_INVENTORY_IMAGE_REQUEST_NAME,
    gents_protocol::schemas::FILE_INVENTORY_REQUEST_NAME,
    gents_protocol::schemas::AGENT_SESSION_INPUT_EDIT_NAME,
    gents_protocol::schemas::AGENT_SESSION_GOAL_CONTROL_NAME,
    gents_protocol::schemas::AGENT_SESSION_GOAL_RECEIPT_NAME,
    gents_protocol::schemas::AGENT_TOOL_CONTROL_NAME,
    gents_protocol::schemas::NODE_DIRECTORY_ENTRY_NAME,
    gents_protocol::schemas::MAILBOX_ITEM_NAME,
];
