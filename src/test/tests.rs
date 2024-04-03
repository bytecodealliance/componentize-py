use {
    super::{Ctx, Tester, SEED},
    anyhow::{anyhow, Error, Result},
    async_trait::async_trait,
    once_cell::sync::Lazy,
    std::str,
    wasmtime::{
        component::{Instance, InstancePre, Linker, Resource, ResourceAny},
        Store,
    },
    wasmtime_wasi::{
        preview2::{command, DirPerms, FilePerms, WasiCtxBuilder, WasiView},
        Dir,
    },
};

wasmtime::component::bindgen!({
    path: "src/test/wit",
    world: "tests",
    async: true,
    with: {
        "componentize-py:test/resource-import-and-export/thing": ThingU32,
        "componentize-py:test/resource-borrow-import/thing": ThingU32,
        "componentize-py:test/resource-borrow-export/thing": ThingU32,
        "componentize-py:test/resource-with-lists/thing": ThingList,
        "componentize-py:test/resource-aggregates/thing": ThingU32,
        "componentize-py:test/resource-alias1/thing": ThingString,
        "componentize-py:test/resource-floats/float": MyFloat,
        "resource-floats-imports/float": MyFloat,
        "resource-floats-exports/float": MyFloat,
        "componentize-py:test/resource-borrow-in-record/thing": ThingString,
    },
});

mod foo_sdk {
    wasmtime::component::bindgen!({
        path: "src/test/foo_sdk/wit",
        world: "foo-world",
        async: {
            only_imports: [],
        },
    });
}

mod bar_sdk {
    wasmtime::component::bindgen!({
        path: "src/test/bar_sdk/wit",
        world: "bar-world",
        async: true,
        with: {
            "foo:sdk/foo-interface": super::foo_sdk::foo::sdk::foo_interface,
        },
    });
}

pub struct ThingU32(u32);
pub struct ThingList(Vec<u8>);
pub struct ThingString(String);
pub struct MyFloat(f64);

macro_rules! load_guest_code {
    ($($input_string:expr),*) => {
        &[
            $(
                ($input_string, include_str!(concat!("./python_source/", $input_string))),
            )*
        ]
    };
}

const GUEST_CODE: &[(&str, &str)] = load_guest_code!(
    "app.py",
    "resource_import_and_export.py",
    "resource_borrow_export.py",
    "resource_with_lists.py",
    "resource_aggregates.py",
    "resource_alias1.py",
    "resource_floats_exports.py",
    "resource_borrow_in_record.py"
);

struct Host;

#[async_trait]
impl super::Host for Host {
    type World = Tests;

    fn add_to_linker(linker: &mut Linker<Ctx>) -> Result<()> {
        command::add_to_linker(linker)?;
        Tests::add_to_linker(linker, |ctx| ctx)?;
        foo_sdk::FooWorld::add_to_linker(linker, |ctx| ctx)?;
        Ok(())
    }

    async fn instantiate_pre(
        store: &mut Store<Ctx>,
        pre: &InstancePre<Ctx>,
    ) -> Result<(Self::World, Instance)> {
        Ok(Tests::instantiate_pre(store, pre).await?)
    }
}

struct FooHost;

#[async_trait]
impl super::Host for FooHost {
    type World = foo_sdk::FooWorld;

    fn add_to_linker(_linker: &mut Linker<Ctx>) -> Result<()> {
        unreachable!()
    }

    async fn instantiate_pre(
        store: &mut Store<Ctx>,
        pre: &InstancePre<Ctx>,
    ) -> Result<(Self::World, Instance)> {
        Ok(foo_sdk::FooWorld::instantiate_pre(store, pre).await?)
    }
}

struct BarHost;

#[async_trait]
impl super::Host for BarHost {
    type World = bar_sdk::BarWorld;

    fn add_to_linker(_linker: &mut Linker<Ctx>) -> Result<()> {
        unreachable!()
    }

    async fn instantiate_pre(
        store: &mut Store<Ctx>,
        pre: &InstancePre<Ctx>,
    ) -> Result<(Self::World, Instance)> {
        Ok(bar_sdk::BarWorld::instantiate_pre(store, pre).await?)
    }
}

static TESTER: Lazy<Tester<Host>> = Lazy::new(|| {
    Tester::<Host>::new(
        include_str!("wit/tests.wit"),
        GUEST_CODE,
        &["src/test"],
        &[("foo_sdk", "foo-world"), ("bar_sdk", "bar-world")],
        *SEED,
    )
    .unwrap()
});

#[test]
fn simple_export() -> Result<()> {
    TESTER.test(|world, store, runtime| {
        assert_eq!(
            42 + 3,
            runtime.block_on(
                world
                    .componentize_py_test_simple_export()
                    .call_foo(store, 42)
            )?
        );

        Ok(())
    })
}

#[test]
fn simple_import_and_export() -> Result<()> {
    #[async_trait]
    impl componentize_py::test::simple_import_and_export::Host for Ctx {
        async fn foo(&mut self, v: u32) -> Result<u32> {
            Ok(v + 2)
        }
    }

    TESTER.test(|world, store, runtime| {
        assert_eq!(
            42 + 2 + 3,
            runtime.block_on(
                world
                    .componentize_py_test_simple_import_and_export()
                    .call_foo(store, 42)
            )?
        );

        Ok(())
    })
}

#[test]
fn resource_import_and_export() -> Result<()> {
    use componentize_py::test::resource_import_and_export::{Host, HostThing};

    #[async_trait]
    impl HostThing for Ctx {
        async fn new(&mut self, v: u32) -> Result<Resource<ThingU32>> {
            Ok(self.table().push(ThingU32(v + 8))?)
        }

        async fn foo(&mut self, this: Resource<ThingU32>) -> Result<u32> {
            Ok(self.table().get(&this)?.0 + 1)
        }

        async fn bar(&mut self, this: Resource<ThingU32>, v: u32) -> Result<()> {
            self.table().get_mut(&this)?.0 = v + 5;
            Ok(())
        }

        async fn baz(
            &mut self,
            a: Resource<ThingU32>,
            b: Resource<ThingU32>,
        ) -> Result<Resource<ThingU32>> {
            let a = self.table().get(&a)?.0;
            let b = self.table().get(&b)?.0;

            Ok(self.table().push(ThingU32(a + b + 6))?)
        }

        fn drop(&mut self, this: Resource<ThingU32>) -> Result<()> {
            Ok(self.table().delete(this).map(|_| ())?)
        }
    }

    impl Host for Ctx {}

    TESTER.test(|world, store, runtime| {
        runtime.block_on(async {
            let instance = world.componentize_py_test_resource_import_and_export();
            let thing = instance.thing();
            let thing1 = thing.call_constructor(&mut *store, 42).await?;

            assert_eq!(
                42 + 7 + 8 + 3 + 1,
                thing.call_foo(&mut *store, thing1).await?
            );

            thing.call_bar(&mut *store, thing1, 33).await?;

            assert_eq!(
                33 + 4 + 5 + 3 + 1,
                thing.call_foo(&mut *store, thing1).await?
            );

            let thing2 = thing.call_constructor(&mut *store, 81).await?;

            let thing3 = thing.call_baz(&mut *store, thing1, thing2).await?;

            assert_eq!(
                33 + 4 + 5 + 81 + 7 + 8 + 6 + 1 + 9 + 7 + 8 + 3 + 1,
                thing.call_foo(&mut *store, thing3).await?
            );

            Ok(())
        })
    })
}

#[test]
fn resource_borrow_import() -> Result<()> {
    use componentize_py::test::resource_borrow_import::{Host, HostThing};

    #[async_trait]
    impl HostThing for Ctx {
        async fn new(&mut self, v: u32) -> Result<Resource<ThingU32>> {
            Ok(self.table().push(ThingU32(v + 2))?)
        }

        fn drop(&mut self, this: Resource<ThingU32>) -> Result<()> {
            Ok(self.table().delete(this).map(|_| ())?)
        }
    }

    #[async_trait]
    impl Host for Ctx {
        async fn foo(&mut self, this: Resource<ThingU32>) -> Result<u32> {
            Ok(self.table().get(&this)?.0 + 3)
        }
    }

    TESTER.test(|world, store, runtime| {
        assert_eq!(
            42 + 1 + 2 + 3 + 4,
            runtime.block_on(world.call_test_resource_borrow_import(store, 42))?
        );

        Ok(())
    })
}

#[test]
fn resource_borrow_export() -> Result<()> {
    TESTER.test(|world, store, runtime| {
        runtime.block_on(async {
            let instance = world.componentize_py_test_resource_borrow_export();
            let thing = instance.thing();
            let thing1 = thing.call_constructor(&mut *store, 42).await?;

            assert_eq!(42 + 1 + 2, instance.call_foo(&mut *store, thing1).await?);

            Ok(())
        })
    })
}

#[test]
fn resource_with_lists() -> Result<()> {
    use componentize_py::test::resource_with_lists::{Host, HostThing};

    #[async_trait]
    impl HostThing for Ctx {
        async fn new(&mut self, mut v: Vec<u8>) -> Result<Resource<ThingList>> {
            v.extend(b" HostThing.new");
            Ok(self.table().push(ThingList(v))?)
        }

        async fn foo(&mut self, this: Resource<ThingList>) -> Result<Vec<u8>> {
            let mut v = self.table().get(&this)?.0.clone();
            v.extend(b" HostThing.foo");
            Ok(v)
        }

        async fn bar(&mut self, this: Resource<ThingList>, mut v: Vec<u8>) -> Result<()> {
            v.extend(b" HostThing.bar");
            self.table().get_mut(&this)?.0 = v;
            Ok(())
        }

        async fn baz(&mut self, mut v: Vec<u8>) -> Result<Vec<u8>> {
            v.extend(b" HostThing.baz");
            Ok(v)
        }

        fn drop(&mut self, this: Resource<ThingList>) -> Result<()> {
            Ok(self.table().delete(this).map(|_| ())?)
        }
    }

    impl Host for Ctx {}

    TESTER.test(|world, store, runtime| {
        runtime.block_on(async {
            let instance = world.componentize_py_test_resource_with_lists();
            let thing = instance.thing();
            let thing1 = thing.call_constructor(&mut *store, b"Hi").await?;

            assert_eq!(
                "Hi Thing.__init__ HostThing.new HostThing.foo Thing.foo",
                str::from_utf8(&thing.call_foo(&mut *store, thing1).await?)?
            );

            thing.call_bar(&mut *store, thing1, b"Hola").await?;

            assert_eq!(
                "Hola Thing.bar HostThing.bar HostThing.foo Thing.foo",
                str::from_utf8(&thing.call_foo(&mut *store, thing1).await?)?
            );

            assert_eq!(
                "Ohayo Gozaimas Thing.baz HostThing.baz Thing.baz again",
                str::from_utf8(&thing.call_baz(&mut *store, b"Ohayo Gozaimas").await?)?
            );

            Ok(())
        })
    })
}

#[test]
fn resource_aggregates() -> Result<()> {
    {
        use componentize_py::test::resource_aggregates::{
            Host, HostThing, L1, L2, R1, R2, R3, T1, T2, V1, V2,
        };

        #[async_trait]
        impl HostThing for Ctx {
            async fn new(&mut self, v: u32) -> Result<Resource<ThingU32>> {
                Ok(self.table().push(ThingU32(v + 2))?)
            }

            fn drop(&mut self, this: Resource<ThingU32>) -> Result<()> {
                Ok(self.table().delete(this).map(|_| ())?)
            }
        }

        #[async_trait]
        impl Host for Ctx {
            async fn foo(
                &mut self,
                r1: R1,
                r2: R2,
                r3: R3,
                t1: T1,
                t2: T2,
                v1: V1,
                v2: V2,
                l1: L1,
                l2: L2,
                o1: Option<Resource<ThingU32>>,
                o2: Option<Resource<ThingU32>>,
                result1: Result<Resource<ThingU32>, ()>,
                result2: Result<Resource<ThingU32>, ()>,
            ) -> Result<u32> {
                let V1::Thing(v1) = v1;
                let V2::Thing(v2) = v2;
                Ok(self.table().get(&r1.thing)?.0
                    + self.table().get(&r2.thing)?.0
                    + self.table().get(&r3.thing1)?.0
                    + self.table().get(&r3.thing2)?.0
                    + self.table().get(&t1.0)?.0
                    + self.table().get(&t1.1.thing)?.0
                    + self.table().get(&t2.0)?.0
                    + self.table().get(&v1)?.0
                    + self.table().get(&v2)?.0
                    + l1.into_iter()
                        .try_fold(0, |n, v| Ok::<_, Error>(self.table().get(&v)?.0 + n))?
                    + l2.into_iter()
                        .try_fold(0, |n, v| Ok::<_, Error>(self.table().get(&v)?.0 + n))?
                    + o1.map(|v| Ok::<_, Error>(self.table().get(&v)?.0))
                        .unwrap_or(Ok(0))?
                    + o2.map(|v| Ok::<_, Error>(self.table().get(&v)?.0))
                        .unwrap_or(Ok(0))?
                    + result1
                        .map(|v| Ok::<_, Error>(self.table().get(&v)?.0))
                        .unwrap_or(Ok(0))?
                    + result2
                        .map(|v| Ok::<_, Error>(self.table().get(&v)?.0))
                        .unwrap_or(Ok(0))?
                    + 3)
            }
        }
    }

    use exports::componentize_py::test::resource_aggregates::{R1, R2, R3, V1, V2};

    TESTER.test(|world, store, runtime| {
        runtime.block_on(async {
            let instance = world.componentize_py_test_resource_aggregates();
            let thing = instance.thing();

            let mut things = Vec::new();
            for n in 1..18 {
                things.push(thing.call_constructor(&mut *store, n).await?);
            }

            assert_eq!(
                (1..18).map(|n| n + 1 + 2).sum::<u32>() + 3 + 4,
                instance
                    .call_foo(
                        &mut *store,
                        R1 { thing: things[0] },
                        R2 { thing: things[1] },
                        R3 {
                            thing1: things[2],
                            thing2: things[3]
                        },
                        (things[4], R1 { thing: things[5] }),
                        (things[6],),
                        V1::Thing(things[7]),
                        V2::Thing(things[8]),
                        &vec![things[9], things[10]],
                        &vec![things[11], things[12]],
                        Some(things[13]),
                        Some(things[14]),
                        Ok(things[15]),
                        Ok(things[16])
                    )
                    .await?
            );

            Ok(())
        })
    })
}

#[test]
fn resource_alias() -> Result<()> {
    {
        use componentize_py::test::resource_alias1::{Foo, Host, HostThing};

        #[async_trait]
        impl HostThing for Ctx {
            async fn new(&mut self, s: String) -> Result<Resource<ThingString>> {
                Ok(self.table().push(ThingString(s + " HostThing::new"))?)
            }

            async fn get(&mut self, this: Resource<ThingString>) -> Result<String> {
                Ok(format!("{} HostThing.get", self.table().get(&this)?.0))
            }

            fn drop(&mut self, this: Resource<ThingString>) -> Result<()> {
                Ok(self.table().delete(this).map(|_| ())?)
            }
        }

        #[async_trait]
        impl Host for Ctx {
            async fn a(&mut self, f: Foo) -> Result<Vec<Resource<ThingString>>> {
                Ok(vec![f.thing])
            }
        }
    }

    {
        use componentize_py::test::resource_alias2::{Bar, Foo, Host, Thing};

        #[async_trait]
        impl Host for Ctx {
            async fn b(&mut self, f: Foo, g: Bar) -> Result<Vec<Resource<Thing>>> {
                Ok(vec![f.thing, g.thing])
            }
        }
    }

    use exports::componentize_py::test::{resource_alias1, resource_alias2};

    TESTER.test(|world, store, runtime| {
        runtime.block_on(async {
            let thing1 = store
                .data_mut()
                .table()
                .push(ThingString("Ni Hao".to_string()))?;

            fn host_things_to_strings(
                store: &mut Store<Ctx>,
                things: Vec<Resource<ThingString>>,
            ) -> Result<Vec<String>> {
                let mut strings = Vec::new();
                for thing in things {
                    strings.push(store.data_mut().table().get(&thing)?.0.clone());
                }

                Ok(strings)
            }

            let things = world
                .call_test_resource_alias(&mut *store, &[thing1])
                .await?;

            assert_eq!(
                vec!["Ni Hao".to_string()],
                host_things_to_strings(&mut *store, things)?
            );

            let instance = world.componentize_py_test_resource_alias1();
            let thing = instance.thing();
            let thing1 = thing.call_constructor(&mut *store, "Ciao").await?;

            async fn guest_things_to_strings(
                store: &mut Store<Ctx>,
                thing: &resource_alias1::GuestThing<'_>,
                things: Vec<ResourceAny>,
            ) -> Result<Vec<String>> {
                let mut strings = Vec::new();
                for t in things {
                    strings.push(thing.call_get(&mut *store, t).await?);
                }

                Ok(strings)
            }

            let things = instance
                .call_a(&mut *store, resource_alias1::Foo { thing: thing1 })
                .await?;

            assert_eq!(
                vec!["Ciao Thing.__init__ HostThing::new HostThing.get Thing.get".to_string()],
                guest_things_to_strings(&mut *store, &thing, things).await?
            );

            let instance = world.componentize_py_test_resource_alias2();
            let thing2 = thing.call_constructor(&mut *store, "Aloha").await?;

            let things = instance
                .call_b(
                    &mut *store,
                    resource_alias2::Foo { thing: thing1 },
                    resource_alias2::Bar { thing: thing2 },
                )
                .await?;

            assert_eq!(
                vec![
                    "Ciao Thing.__init__ HostThing::new HostThing.get Thing.get".to_string(),
                    "Aloha Thing.__init__ HostThing::new HostThing.get Thing.get".to_string()
                ],
                guest_things_to_strings(&mut *store, &thing, things).await?
            );

            Ok(())
        })
    })
}

#[test]
fn resource_floats() -> Result<()> {
    {
        use resource_floats_imports::{Host, HostFloat};

        #[async_trait]
        impl HostFloat for Ctx {
            async fn new(&mut self, v: f64) -> Result<Resource<MyFloat>> {
                Ok(self.table().push(MyFloat(v + 2_f64))?)
            }

            async fn get(&mut self, this: Resource<MyFloat>) -> Result<f64> {
                Ok(self.table().get(&this)?.0 + 4_f64)
            }

            async fn add(&mut self, a: Resource<MyFloat>, b: f64) -> Result<Resource<MyFloat>> {
                let a = self.table().get(&a)?.0;
                Ok(self.table().push(MyFloat(a + b + 6_f64))?)
            }

            fn drop(&mut self, this: Resource<MyFloat>) -> Result<()> {
                Ok(self.table().delete(this).map(|_| ())?)
            }
        }

        impl Host for Ctx {}
    }

    {
        use componentize_py::test::resource_floats::{Host, HostFloat};

        #[async_trait]
        impl HostFloat for Ctx {
            async fn new(&mut self, v: f64) -> Result<Resource<MyFloat>> {
                Ok(self.table().push(MyFloat(v + 1_f64))?)
            }

            async fn get(&mut self, this: Resource<MyFloat>) -> Result<f64> {
                Ok(self.table().get(&this)?.0 + 3_f64)
            }

            fn drop(&mut self, this: Resource<MyFloat>) -> Result<()> {
                Ok(self.table().delete(this).map(|_| ())?)
            }
        }

        impl Host for Ctx {}
    }

    TESTER.test(|world, store, runtime| {
        runtime.block_on(async {
            let float1 = store.data_mut().table().push(MyFloat(42_f64))?;
            let float2 = store.data_mut().table().push(MyFloat(55_f64))?;
            let sum = world.call_add(&mut *store, float1, float2).await?;

            assert_eq!(
                42_f64 + 3_f64 + 55_f64 + 3_f64 + 5_f64 + 1_f64,
                store.data_mut().table().get(&sum)?.0
            );

            let instance = world.resource_floats_exports();
            let float = instance.float();
            let float1 = float.call_constructor(&mut *store, 22_f64).await?;

            assert_eq!(
                22_f64 + 1_f64 + 2_f64 + 4_f64 + 3_f64,
                float.call_get(&mut *store, float1).await?
            );

            let result = float.call_add(&mut *store, float1, 7_f64).await?;

            assert_eq!(
                22_f64
                    + 1_f64
                    + 2_f64
                    + 7_f64
                    + 6_f64
                    + 4_f64
                    + 5_f64
                    + 1_f64
                    + 2_f64
                    + 4_f64
                    + 3_f64,
                float.call_get(&mut *store, result).await?
            );

            Ok(())
        })
    })
}

#[test]
fn resource_borrow_in_record() -> Result<()> {
    {
        use componentize_py::test::resource_borrow_in_record::{Foo, Host, HostThing};

        #[async_trait]
        impl HostThing for Ctx {
            async fn new(&mut self, v: String) -> Result<Resource<ThingString>> {
                Ok(self.table().push(ThingString(v + " HostThing::new"))?)
            }

            async fn get(&mut self, this: Resource<ThingString>) -> Result<String> {
                Ok(format!("{} HostThing.get", self.table().get(&this)?.0))
            }

            fn drop(&mut self, this: Resource<ThingString>) -> Result<()> {
                Ok(self.table().delete(this).map(|_| ())?)
            }
        }

        #[async_trait]
        impl Host for Ctx {
            async fn test(&mut self, list: Vec<Foo>) -> Result<Vec<Resource<ThingString>>> {
                list.into_iter()
                    .map(|foo| {
                        let value = self.table().get(&foo.thing)?.0.clone();
                        Ok(self.table().push(ThingString(value + " HostThing::test"))?)
                    })
                    .collect()
            }
        }
    }

    use exports::componentize_py::test::resource_borrow_in_record::{Foo, GuestThing};

    TESTER.test(|world, store, runtime| {
        runtime.block_on(async {
            let instance = world.componentize_py_test_resource_borrow_in_record();
            let thing = instance.thing();
            let thing1 = thing.call_constructor(&mut *store, "Bonjour").await?;
            let thing2 = thing.call_constructor(&mut *store, "mon cher").await?;

            let things = instance
                .call_test(&mut *store, &[Foo { thing: thing1 }, Foo { thing: thing2 }])
                .await?;

            async fn things_to_strings(
                store: &mut Store<Ctx>,
                thing: &GuestThing<'_>,
                things: Vec<ResourceAny>,
            ) -> Result<Vec<String>> {
                let mut strings = Vec::new();
                for t in things {
                    strings.push(thing.call_get(&mut *store, t).await?);
                }

                Ok(strings)
            }

            assert_eq!(
                vec![
                    "Bonjour Thing.__init__ HostThing::new \
                     HostThing::test HostThing.get Thing.get"
                        .to_string(),
                    "mon cher Thing.__init__ HostThing::new \
                     HostThing::test HostThing.get Thing.get"
                        .to_string()
                ],
                things_to_strings(&mut *store, &thing, things).await?
            );

            Ok(())
        })
    })
}

#[test]
fn multiworld() -> Result<()> {
    impl foo_sdk::foo::sdk::foo_interface::Host for Ctx {
        fn test(&mut self, s: String) -> Result<String> {
            Ok(format!("{s} HostFoo::test"))
        }
    }

    TESTER.test_with::<FooHost>(|world, store, runtime| {
        runtime.block_on(async {
            let result = world
                .foo_sdk_foo_interface()
                .call_test(store, "Howdy")
                .await?;

            assert_eq!("Howdy FooInterface.test HostFoo::test", result);

            Ok(())
        })
    })?;

    TESTER.test_with::<BarHost>(|world, store, runtime| {
        runtime.block_on(async {
            let result = world
                .bar_sdk_bar_interface()
                .call_test(store, "Howdy")
                .await?;

            assert_eq!("Howdy BarInterface.test HostFoo::test", result);

            Ok(())
        })
    })
}

#[test]
fn filesystem() -> Result<()> {
    let filename = "foo.txt";
    let message = b"The Jabberwock, with eyes of flame";

    let dir = tempfile::tempdir()?;
    std::fs::write(dir.path().join(filename), message)?;
    let wasi = WasiCtxBuilder::new()
        .inherit_stdout()
        .inherit_stderr()
        .preopened_dir(
            Dir::open_ambient_dir(dir.path(), cap_std::ambient_authority())?,
            DirPerms::all(),
            FilePerms::all(),
            "/",
        )
        .build();

    TESTER.test_with_wasi::<Host>(wasi, |world, store, runtime| {
        runtime.block_on(async {
            let value = world
                .call_read_file(store, filename)
                .await?
                .map_err(|s| anyhow!("{s}"))?;

            assert_eq!(&value, message);

            Ok(())
        })
    })
}
